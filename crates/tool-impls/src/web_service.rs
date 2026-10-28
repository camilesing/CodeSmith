//! Reqwest-backed [`WebService`] provider — the implementation half of the
//! web-service seam (TODO-49; definition in
//! `codesmith-agent-runtime::web_service`).
//!
//! `HttpWebService` owns every web transport the tools use:
//!
//! - **search** dispatches on the configured [`SearchProvider`] (from
//!   `[search] provider` in config.toml — switching backends is config-only):
//!   the API providers (Tavily / Bocha / Metaso / Baidu / Volcengine), the
//!   active provider's own server-side `web_search` tool
//!   (`provider_native`), and the DuckDuckGo / Bing HTML scrape, ported from
//!   `crates/cli/src/tools/web_search.rs`.
//! - **fetch** is anonymous HTTP with the SSRF validation (restricted-IP
//!   block, DNS resolution check, DNS-pinning against rebinding) and the
//!   policy-rechecked redirect loop, ported from
//!   `crates/tool-impls/src/tools/fetch_url.rs` and unified with
//!   `web_run.rs`'s page fetch.
//!
//! # Behavior convergences over the three predecessors
//!
//! - `web.run`'s text search now follows `[search] provider` (it was
//!   hardwired to DuckDuckGo+Bing) and its page fetch now runs the same SSRF
//!   validation as `fetch_url` (it previously had none) — the tools'
//!   behavior differs only in post-processing, never in transport.
//! - A hard policy deny on the search provider's *primary* host fails the
//!   search; a deny on a *fallback* host degrades to a warning in the
//!   outcome (previously an error in `web_search`, a warning in `web_run`).
//! - One browser-like User-Agent and one Accept header set serve all fetches.
//!
//! # Known limitations
//!
//! - Client instances are built per request (pre-seam behavior kept); there
//!   is no connection pooling across calls.
//! - `web.run`'s `image_query` scrape is not routed through this service.
//! - `provider_native` reads the provider's own search-result envelope and
//!   ignores the assistant's prose summary; a gateway that renames or drops
//!   that envelope yields "no parseable results" rather than a best guess.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose};
use codesmith_agent_runtime::config_types::SearchProvider;
use codesmith_agent_runtime::network_policy::{Decision, NetworkPolicyDecider, host_from_url};
use codesmith_agent_runtime::utils::url_encode;
use codesmith_agent_runtime::web_service::{
    FetchOutcome, FetchRequest, SearchHit, SearchOutcome, SearchRequest, WebError, WebErrorKind,
    WebService,
};
use regex::Regex;
use serde_json::{Value, json};
use std::sync::OnceLock;
use tokio_util::sync::CancellationToken;

const DUCKDUCKGO_HOST: &str = "html.duckduckgo.com";
const BING_HOST: &str = "www.bing.com";
const TAVILY_ENDPOINT: &str = "https://api.tavily.com/search";
const BOCHA_ENDPOINT: &str = "https://api.bochaai.com/v1/ai/search";
const METASO_ENDPOINT: &str = "https://metaso.cn/api/v1";
const BAIDU_ENDPOINT: &str = "https://qianfan.baidubce.com/v2/ai_search/web_search";
const VOLCENGINE_RESPONSES_ENDPOINT: &str = "https://ark.cn-beijing.volces.com/api/v3/responses";
/// Server-side search tool declared to an Anthropic-compatible Messages API
/// (the same tool type ZCode sends). Gateways that predate it answer with an
/// `unknown tool` error, which the tool surfaces verbatim.
const PROVIDER_NATIVE_TOOL_TYPE: &str = "web_search_20260209";
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// The provider-native path pays for a search *and* a generation, so it keeps
/// a floor above the 15 s tool default; a short connect timeout still surfaces
/// a dead endpoint quickly.
const PROVIDER_NATIVE_MIN_TIMEOUT: Duration = Duration::from_secs(60);
const ERROR_BODY_PREVIEW_BYTES: usize = 512;
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";

/// Connection details for `provider_native` search: the active model
/// provider's Anthropic-compatible endpoint, resolved by the host from the
/// `[providers.*]` table (see `Config::web_service`). The tool never reads
/// `[search] api_key` for this backend — the provider key is the only key
/// that authenticates this route.
#[derive(Debug, Clone)]
pub struct NativeSearchConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

/// Reqwest-backed [`WebService`]: config-selected search provider + anonymous
/// HTTP fetch. Constructed by the host from `[search]` config and attached to
/// the engine / tool context.
#[derive(Debug, Clone, Default)]
pub struct HttpWebService {
    provider: SearchProvider,
    api_key: Option<String>,
    native: Option<NativeSearchConfig>,
}

impl HttpWebService {
    /// Build the service for one provider selection. `api_key` is the
    /// `[search] api_key` value; per-provider env-var fallbacks are applied
    /// at request time (Metaso / Baidu / Volcengine).
    #[must_use]
    pub fn new(provider: SearchProvider, api_key: Option<String>) -> Self {
        Self {
            provider,
            api_key,
            native: None,
        }
    }

    /// Attach the provider-native transport used by
    /// [`SearchProvider::ProviderNative`]; without it that backend fails
    /// closed with a configuration error.
    #[must_use]
    pub fn with_native_search(mut self, native: NativeSearchConfig) -> Self {
        self.native = Some(native);
        self
    }
}

#[async_trait]
impl WebService for HttpWebService {
    async fn search(&self, req: SearchRequest) -> Result<SearchOutcome, WebError> {
        race_cancel(req.cancel.as_ref(), self.do_search(&req)).await
    }

    async fn fetch(&self, req: FetchRequest) -> Result<FetchOutcome, WebError> {
        race_cancel(req.cancel.as_ref(), self.do_fetch(&req)).await
    }

    fn search_hosts(&self) -> Vec<String> {
        if self.provider == SearchProvider::ProviderNative {
            // The endpoint is the operator's own gateway, not a fixed vendor
            // host — the approval gate must see whatever `base_url` names.
            return self
                .native
                .as_ref()
                .and_then(|native| host_from_url(native.base_url.trim()))
                .into_iter()
                .collect();
        }
        let hosts: &[&str] = match self.provider {
            SearchProvider::Tavily => &["api.tavily.com"],
            SearchProvider::Bocha => &["api.bochaai.com"],
            SearchProvider::Metaso => &["metaso.cn"],
            SearchProvider::Baidu => &["qianfan.baidubce.com"],
            SearchProvider::Volcengine => &["ark.cn-beijing.volces.com"],
            // The scrape providers can fall through to each other.
            SearchProvider::Bing => &[BING_HOST, DUCKDUCKGO_HOST],
            SearchProvider::DuckDuckGo => &[DUCKDUCKGO_HOST],
            SearchProvider::ProviderNative => unreachable!("handled above"),
        };
        hosts.iter().map(|h| (*h).to_string()).collect()
    }
}

impl HttpWebService {
    async fn do_search(&self, req: &SearchRequest) -> Result<SearchOutcome, WebError> {
        match self.provider {
            SearchProvider::Tavily => self.run_tavily_search(req).await,
            SearchProvider::Bocha => self.run_bocha_search(req).await,
            SearchProvider::Metaso => self.run_metaso_search(req).await,
            SearchProvider::Baidu => self.run_baidu_search(req).await,
            SearchProvider::Volcengine => self.run_volcengine_search(req).await,
            SearchProvider::ProviderNative => self.run_provider_native_search(req).await,
            SearchProvider::Bing | SearchProvider::DuckDuckGo => self.run_scrape_search(req).await,
        }
    }

    // === fetch ============================================================

    async fn do_fetch(&self, req: &FetchRequest) -> Result<FetchOutcome, WebError> {
        let mut current_url = reqwest::Url::parse(req.url.trim())
            .map_err(|e| WebError::new(WebErrorKind::InvalidUrl, format!("invalid URL: {e}")))?;
        let mut redirects_followed = 0usize;

        loop {
            // Per-hop validation: network policy, then SSRF checks with DNS
            // pinning (a denied host must not be reachable by redirect from
            // an allowed one).
            let dns_pinning = validate_fetch_target(&current_url, req).await?;

            let mut client_builder = reqwest::Client::builder()
                .timeout(req.timeout)
                .user_agent(USER_AGENT)
                .redirect(reqwest::redirect::Policy::none());
            if let Some((hostname, validated_ip)) = dns_pinning {
                client_builder =
                    client_builder.resolve(&hostname, SocketAddr::new(validated_ip, 0));
            }
            let client = client_builder.build().map_err(|e| {
                WebError::new(
                    WebErrorKind::Transport,
                    format!("failed to build HTTP client: {e}"),
                )
            })?;

            let resp = client
                .get(current_url.clone())
                .header(
                    "Accept",
                    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                )
                .header("Accept-Language", "en-US,en;q=0.5")
                .send()
                .await
                .map_err(|e| reqwest_web_error("request failed", &e))?;

            if !resp.status().is_redirection() || redirects_followed >= req.max_redirects {
                return fetch_outcome_from(resp, req.max_bytes).await;
            }

            let Some(location) = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
            else {
                return fetch_outcome_from(resp, req.max_bytes).await;
            };

            current_url = resp.url().join(location).map_err(|e| {
                WebError::new(
                    WebErrorKind::Transport,
                    format!("invalid redirect location: {e}"),
                )
            })?;
            redirects_followed += 1;
        }
    }

    // === API-backed search providers ======================================

    /// Search via Tavily AI Search API (<https://tavily.com>).
    async fn run_tavily_search(&self, req: &SearchRequest) -> Result<SearchOutcome, WebError> {
        check_policy(req.policy.as_ref(), "api.tavily.com", req.tool)?;
        let api_key = self.api_key.as_deref().ok_or_else(|| {
            WebError::new(
                WebErrorKind::Config,
                "Tavily search requires an API key. Set `[search] api_key = \"tvly-...\"` in config.toml.",
            )
        })?;

        let client = search_client(req.timeout, None, None)?;
        let payload = json!({
            "api_key": api_key, // noqa: api-key-in-body
            "query": req.query,
            "search_depth": "basic",
            "max_results": req.max_results,
        });

        let resp = client
            .post(TAVILY_ENDPOINT)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| reqwest_web_error("Tavily search request failed", &e))?;
        let (status, body) = read_body("Failed to read Tavily response", resp).await?;
        if !status.is_success() {
            return Err(WebError::new(
                WebErrorKind::Status(status.as_u16()),
                format!(
                    "Tavily search failed: HTTP {} — {}",
                    status.as_u16(),
                    truncate_error_body(&body)
                ),
            ));
        }

        let parsed: Value = serde_json::from_str(&body).map_err(|e| {
            WebError::new(
                WebErrorKind::Body,
                format!("Failed to parse Tavily response: {e}"),
            )
        })?;

        let results = parse_generic_results(
            &parsed,
            &[&["title"], &["url"], &["content", "snippet"]],
            req.max_results,
        );
        Ok(SearchOutcome {
            results,
            source: "tavily".to_string(),
            warnings: Vec::new(),
        })
    }

    /// Search via Bocha AI Search API (<https://bochaai.com>).
    async fn run_bocha_search(&self, req: &SearchRequest) -> Result<SearchOutcome, WebError> {
        check_policy(req.policy.as_ref(), "api.bochaai.com", req.tool)?;
        let api_key = self.api_key.as_deref().ok_or_else(|| {
            WebError::new(
                WebErrorKind::Config,
                "Bocha search requires an API key. Set `[search] api_key = \"sk-...\"` in config.toml.",
            )
        })?;

        let client = search_client(req.timeout, None, None)?;
        let payload = json!({
            "query": req.query,
            "freshness": "noLimit",
            "count": req.max_results,
        });

        let resp = client
            .post(BOCHA_ENDPOINT)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&payload)
            .send()
            .await
            .map_err(|e| reqwest_web_error("Bocha search request failed", &e))?;
        let (status, body) = read_body("Failed to read Bocha response", resp).await?;
        if !status.is_success() {
            return Err(WebError::new(
                WebErrorKind::Status(status.as_u16()),
                format!(
                    "Bocha search failed: HTTP {} — {}",
                    status.as_u16(),
                    truncate_error_body(&body)
                ),
            ));
        }

        let parsed: Value = serde_json::from_str(&body).map_err(|e| {
            WebError::new(
                WebErrorKind::Body,
                format!("Failed to parse Bocha response: {e}"),
            )
        })?;

        // Bocha returns `{"code": 200, "data": {"pages": [...]}}`
        let pages = parsed
            .get("data")
            .and_then(|d| d.get("pages"))
            .or_else(|| parsed.get("pages"))
            .cloned()
            .unwrap_or(Value::Null);
        let results = parse_generic_results(
            &pages,
            &[
                &["name", "title"],
                &["url", "link"],
                &["summary", "snippet", "description"],
            ],
            req.max_results,
        );
        Ok(SearchOutcome {
            results,
            source: "bocha".to_string(),
            warnings: Vec::new(),
        })
    }

    /// Search via Metaso AI Search API (<https://metaso.cn>). Requires
    /// `[search] api_key` or `METASO_API_KEY` — the community default key
    /// that used to be compiled in as a last resort was removed: one shared
    /// key across every install exhausted its quota collectively and shipped
    /// an implicit credential in the binary.
    async fn run_metaso_search(&self, req: &SearchRequest) -> Result<SearchOutcome, WebError> {
        check_policy(req.policy.as_ref(), "metaso.cn", req.tool)?;
        let env_key = std::env::var("METASO_API_KEY").ok();
        let api_key = self
            .api_key
            .as_deref()
            .or(env_key.as_deref())
            .ok_or_else(|| {
                WebError::new(
                    WebErrorKind::Config,
                    "Metaso search requires an API key. Set METASO_API_KEY or `[search] api_key = \"…\"` in config.toml, or get one at https://metaso.cn/search-api/playground",
                )
            })?;

        let client = search_client(req.timeout, None, None)?;
        let size = req.max_results.clamp(1, 100);
        let payload = json!({
            "q": req.query,
            "scope": "webpage",
            "size": size,
        });

        let resp = client
            .post(format!("{METASO_ENDPOINT}/search"))
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&payload)
            .send()
            .await
            .map_err(|e| reqwest_web_error("Metaso search request failed", &e))?;
        let (status, body) = read_body("Failed to read Metaso response", resp).await?;
        if !status.is_success() {
            let msg = match status.as_u16() {
                401 | 403 => "Metaso API key rejected — check METASO_API_KEY or set `[search] api_key` in config.toml, or get one at https://metaso.cn/search-api/playground".to_string(),
                429 => "Metaso rate-limited — wait and retry, or get your own API key at https://metaso.cn/search-api/playground".to_string(),
                _ => format!(
                    "Metaso server error (HTTP {status}) — {}",
                    truncate_error_body(&body)
                ),
            };
            return Err(WebError::new(WebErrorKind::Status(status.as_u16()), msg));
        }

        let parsed: Value = serde_json::from_str(&body).map_err(|e| {
            WebError::new(
                WebErrorKind::Body,
                format!("Failed to parse Metaso response: {e}"),
            )
        })?;

        // Check business-logic error codes in the response body.
        if let Some(code) = parsed.get("code").and_then(|v| v.as_i64())
            && code != 0
        {
            let msg = parsed
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            return Err(WebError::new(
                WebErrorKind::Status(status.as_u16()),
                match code {
                    3003 => "Metaso: daily search limit reached — set METASO_API_KEY or get one at https://metaso.cn/search-api/playground".to_string(),
                    2005 => "Metaso API key rejected — check METASO_API_KEY or set `[search] api_key` in config.toml".to_string(),
                    _ => format!("Metaso API error (code {code}: {msg})"),
                },
            ));
        }

        let results = parse_generic_results(
            &parsed,
            &[&["title"], &["link"], &["snippet", "summary"]],
            size,
        );
        Ok(SearchOutcome {
            results,
            source: "metaso".to_string(),
            warnings: Vec::new(),
        })
    }

    /// Search via Baidu AI Search API (<https://qianfan.baidubce.com>).
    async fn run_baidu_search(&self, req: &SearchRequest) -> Result<SearchOutcome, WebError> {
        check_policy(req.policy.as_ref(), "qianfan.baidubce.com", req.tool)?;
        let env_key = std::env::var("BAIDU_SEARCH_API_KEY").ok();
        let api_key = self
            .api_key
            .as_deref()
            .or(env_key.as_deref())
            .ok_or_else(|| {
                WebError::new(
                    WebErrorKind::Config,
                    "Baidu search requires an API key. Set `BAIDU_SEARCH_API_KEY` or `[search] api_key` in config.toml.",
                )
            })?;

        let client = search_client(req.timeout, None, None)?;
        let payload = baidu_search_payload(&req.query, req.max_results);

        let resp = client
            .post(BAIDU_ENDPOINT)
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&payload)
            .send()
            .await
            .map_err(|e| reqwest_web_error("Baidu search request failed", &e))?;
        let (status, body) = read_body("Failed to read Baidu response", resp).await?;
        if !status.is_success() {
            let msg = match status.as_u16() {
                401 | 403 => "Baidu search API key rejected — check BAIDU_SEARCH_API_KEY or `[search] api_key` in config.toml".to_string(),
                429 => "Baidu search rate-limited — wait and retry, or check your Baidu AI Search quota".to_string(),
                _ => format!(
                    "Baidu search failed: HTTP {} — {}",
                    status.as_u16(),
                    truncate_error_body(&body)
                ),
            };
            return Err(WebError::new(WebErrorKind::Status(status.as_u16()), msg));
        }

        let parsed: Value = serde_json::from_str(&body).map_err(|e| {
            WebError::new(
                WebErrorKind::Body,
                format!("Failed to parse Baidu response: {e}"),
            )
        })?;
        if let Some(error) = baidu_error_message(&parsed) {
            return Err(WebError::new(WebErrorKind::Status(status.as_u16()), error));
        }

        let results = parse_generic_results(
            &parsed,
            &[
                &["title", "name"],
                &["url", "link"],
                &["content", "snippet", "summary"],
            ],
            req.max_results,
        );
        Ok(SearchOutcome {
            results,
            source: "baidu".to_string(),
            warnings: Vec::new(),
        })
    }

    /// Search via Volcengine Ark Responses API web_search tool.
    /// Uses strict JSON prompt constraints to extract structured results
    /// from the model's search-augmented response.
    ///
    /// Overrides the user-supplied timeout to a minimum of 90 s because the
    /// Responses API pipeline (web search → model inference → JSON generation)
    /// is inherently slower than simple search-API round-trips.  A separate
    /// `connect_timeout` of 15 s lets DNS/TLS failures surface quickly.
    /// Transient transport errors are retried twice with exponential backoff.
    async fn run_volcengine_search(&self, req: &SearchRequest) -> Result<SearchOutcome, WebError> {
        check_policy(req.policy.as_ref(), "ark.cn-beijing.volces.com", req.tool)?;
        let volc_key = std::env::var("VOLCENGINE_API_KEY").ok();
        let volc_ark_key = std::env::var("VOLCENGINE_ARK_API_KEY").ok();
        let ark_key = std::env::var("ARK_API_KEY").ok();
        let api_key = self
            .api_key
            .as_deref()
            .or(volc_key.as_deref())
            .or(volc_ark_key.as_deref())
            .or(ark_key.as_deref())
            .ok_or_else(|| {
                WebError::new(
                    WebErrorKind::Config,
                    "Volcengine search requires an API key. Set `[search] api_key`, \
                     or VOLCENGINE_API_KEY / VOLCENGINE_ARK_API_KEY / ARK_API_KEY env var.",
                )
            })?;

        let effective_timeout = req.timeout.max(Duration::from_secs(90));
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(effective_timeout)
            .tcp_keepalive(Some(Duration::from_secs(30)))
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| {
                WebError::new(
                    WebErrorKind::Transport,
                    format!("Failed to build HTTP client: {e}"),
                )
            })?;
        let payload = volcengine_search_payload(&req.query, req.max_results);

        // Retry transient transport errors (DNS, connection reset, timeout)
        // up to 2 times with exponential backoff: 1 s, 2 s.
        let mut last_err: Option<WebError> = None;
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(1000 * (1 << (attempt - 1)))).await;
            }

            match client
                .post(VOLCENGINE_RESPONSES_ENDPOINT)
                .header("Authorization", format!("Bearer {api_key}"))
                .json(&payload)
                .send()
                .await
            {
                Ok(resp) => {
                    let (status, body) =
                        read_body("Failed to read Volcengine response", resp).await?;
                    if !status.is_success() {
                        let msg = match status.as_u16() {
                            401 | 403 => "Volcengine API key rejected — check VOLCENGINE_API_KEY or `[search] api_key` in config.toml".to_string(),
                            429 => "Volcengine API rate-limited — wait and retry, or check your quota".to_string(),
                            _ => format!(
                                "Volcengine search failed: HTTP {} — {}",
                                status.as_u16(),
                                truncate_error_body(&body)
                            ),
                        };
                        return Err(WebError::new(WebErrorKind::Status(status.as_u16()), msg));
                    }

                    let parsed: Value = serde_json::from_str(&body).map_err(|e| {
                        WebError::new(
                            WebErrorKind::Body,
                            format!("Failed to parse Volcengine response: {e}"),
                        )
                    })?;
                    if let Some(error) = volcengine_error_message(&parsed) {
                        return Err(WebError::new(WebErrorKind::Status(status.as_u16()), error));
                    }
                    let response_text = volcengine_extract_text(&parsed).ok_or_else(|| {
                        WebError::new(
                            WebErrorKind::Body,
                            "Volcengine response contains no output text",
                        )
                    })?;
                    let results = parse_volcengine_results(&response_text, req.max_results);
                    return Ok(SearchOutcome {
                        results,
                        source: "volcengine".to_string(),
                        warnings: Vec::new(),
                    });
                }
                Err(e) => {
                    let is_transient = e.is_timeout() || e.is_connect();
                    if !is_transient || attempt == 2 {
                        return Err(reqwest_web_error("Volcengine search request failed", &e));
                    }
                    last_err = Some(reqwest_web_error(
                        &format!(
                            "Volcengine search request failed (attempt {}/3)",
                            attempt + 1
                        ),
                        &e,
                    ));
                }
            }
        }

        // Unreachable — the final iteration always returns above.
        Err(last_err.unwrap_or_else(|| {
            WebError::new(
                WebErrorKind::Transport,
                "Volcengine search: unexpected retry exit",
            )
        }))
    }

    // === Provider-native search ==========================================

    /// Search through the active provider's own server-side `web_search`
    /// tool (Anthropic-compatible Messages API) — the ZCode approach: the
    /// vendor runs the search, so there is no SERP to be bot-blocked and no
    /// third-party search key.
    ///
    /// One non-streaming `/v1/messages` call, one server-side tool. Results
    /// are read from `content[]`: BigModel stringifies them (`tool_result`
    /// holding a repr of a list of dicts), Anthropic returns
    /// `web_search_result` objects — [`parse_provider_native_results`]
    /// accepts both. The assistant text is deliberately not parsed; it is a
    /// summary of the results, not the result set.
    async fn run_provider_native_search(
        &self,
        req: &SearchRequest,
    ) -> Result<SearchOutcome, WebError> {
        let native = self.native.as_ref().ok_or_else(|| {
            WebError::new(
                WebErrorKind::Config,
                "`[search] provider = \"provider_native\"` needs an Anthropic-compatible \
                 model provider: set `provider = \"anthropic\"` with `[providers.anthropic]` \
                 `base_url` and `api_key`, or pick another `[search] provider`.",
            )
        })?;

        let host = host_from_url(native.base_url.trim()).unwrap_or_default();
        check_policy(req.policy.as_ref(), &host, req.tool)?;

        let endpoint = messages_endpoint(&native.base_url);
        let client = search_client(
            req.timeout.max(PROVIDER_NATIVE_MIN_TIMEOUT),
            Some(Duration::from_secs(10)),
            None,
        )?;
        let payload = json!({
            "model": native.model,
            "max_tokens": 1024,
            "messages": [{
                "role": "user",
                "content": format!("Perform a web search for the query: {}", req.query),
            }],
            "tools": [{
                "type": PROVIDER_NATIVE_TOOL_TYPE,
                "name": "web_search",
                "max_uses": 2,
            }],
        });

        let resp = client
            .post(&endpoint)
            .header("x-api-key", &native.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| reqwest_web_error("Provider-native web search request failed", &e))?;
        let (status, body) =
            read_body("Failed to read provider-native search response", resp).await?;
        if !status.is_success() {
            let msg = match status.as_u16() {
                401 | 403 => "Provider-native web search rejected the provider API key — \
                              check the active `[providers.*] api_key`."
                    .to_string(),
                429 => "Provider-native web search is rate-limited or this plan has no search \
                        quota — retry later, or pick another `[search] provider`."
                    .to_string(),
                _ => format!(
                    "Provider-native web search failed: HTTP {} — {}",
                    status.as_u16(),
                    truncate_error_body(&body)
                ),
            };
            return Err(WebError::new(WebErrorKind::Status(status.as_u16()), msg));
        }

        let parsed: Value = serde_json::from_str(&body).map_err(|e| {
            WebError::new(
                WebErrorKind::Body,
                format!("Failed to parse provider-native search response: {e}"),
            )
        })?;
        if let Some(error) = provider_native_error_message(&parsed) {
            return Err(WebError::new(WebErrorKind::Status(status.as_u16()), error));
        }

        let results = parse_provider_native_results(&parsed, req.max_results);
        if results.is_empty() {
            return Err(WebError::new(
                WebErrorKind::Body,
                format!(
                    "Provider-native search returned no parseable results — the provider may not \
                     support the `{PROVIDER_NATIVE_TOOL_TYPE}` tool. Response preview: {}",
                    truncate_error_body(&body)
                ),
            ));
        }
        Ok(SearchOutcome {
            results,
            source: "provider_native".to_string(),
            warnings: Vec::new(),
        })
    }

    // === DuckDuckGo / Bing scrape =========================================

    /// HTML-scrape search shared by the Bing and DuckDuckGo providers
    /// (ported from the pre-seam `web_search` tool). A hard policy deny on
    /// the primary host fails the call; a deny on the fallback host degrades
    /// to a warning in the outcome.
    async fn run_scrape_search(&self, req: &SearchRequest) -> Result<SearchOutcome, WebError> {
        let client = search_client(req.timeout, None, Some(USER_AGENT))?;
        let mut warnings: Vec<String> = Vec::new();

        // Track whether Bing was tried and returned zero, so we can surface
        // the fallback in the warnings (#2130).
        let mut bing_was_empty = false;

        if self.provider == SearchProvider::Bing {
            check_policy(req.policy.as_ref(), BING_HOST, req.tool)?;
            let results = run_bing_search(&client, &req.query, req.max_results).await?;
            if !results.is_empty() {
                return Ok(SearchOutcome {
                    results,
                    source: "bing".to_string(),
                    warnings,
                });
            }
            // Bing returned zero results — fall through to DuckDuckGo.
            bing_was_empty = true;
        }

        check_policy(req.policy.as_ref(), DUCKDUCKGO_HOST, req.tool)?;

        let encoded = url_encode(&req.query);
        let ddg_url = format!("https://html.duckduckgo.com/html/?q={encoded}");
        let resp = client
            .get(&ddg_url)
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header("Accept-Language", "en-US,en;q=0.5")
            .send()
            .await
            .map_err(|e| reqwest_web_error("Web search request failed", &e))?;
        let (status, body) = read_body("Failed to read response", resp).await?;
        if !status.is_success() {
            return Err(WebError::new(
                WebErrorKind::Status(status.as_u16()),
                format!("Web search failed: HTTP {}", status.as_u16()),
            ));
        }

        let mut results = parse_duckduckgo_results(&body, req.max_results);
        let mut source = "duckduckgo";

        if bing_was_empty && !results.is_empty() {
            warnings.push("Bing returned no results; used DuckDuckGo fallback".to_string());
        }

        if results.is_empty() {
            let duckduckgo_blocked = is_duckduckgo_challenge(&body);
            // Bing is a separate host — a policy deny on it degrades to a
            // warning (DuckDuckGo already answered; often with zero results)
            // instead of failing the whole search.
            match check_policy(req.policy.as_ref(), BING_HOST, req.tool) {
                Err(policy_err) => {
                    warnings.push(format!("Bing fallback skipped: {policy_err}"));
                }
                Ok(()) => match run_bing_search(&client, &req.query, req.max_results).await {
                    Ok(fallback_results) if !fallback_results.is_empty() => {
                        results = fallback_results;
                        source = "bing";
                        warnings.push(
                            if duckduckgo_blocked {
                                "DuckDuckGo returned a bot challenge; used Bing fallback"
                            } else {
                                "DuckDuckGo returned no parseable results; used Bing fallback"
                            }
                            .to_string(),
                        );
                    }
                    Ok(_) if duckduckgo_blocked => {
                        return Err(WebError::new(
                            WebErrorKind::Body,
                            "DuckDuckGo returned a bot challenge and Bing fallback returned no results",
                        ));
                    }
                    Err(err) if duckduckgo_blocked => {
                        return Err(WebError::new(
                            WebErrorKind::Body,
                            format!(
                                "DuckDuckGo returned a bot challenge and Bing fallback failed: {err}"
                            ),
                        ));
                    }
                    Ok(_) | Err(_) => {}
                },
            }
        }

        Ok(SearchOutcome {
            results,
            source: source.to_string(),
            warnings,
        })
    }
}

// === Shared helpers ========================================================

/// Returns `Ok(())` if the policy allows the call. Falls through silently
/// when no policy is attached (back-compat). A `Prompt` decision passes —
/// the inline approval gate at tool-dispatch time owns the ask (P1-5).
fn check_policy(
    decider: Option<&NetworkPolicyDecider>,
    host: &str,
    tool: &str,
) -> Result<(), WebError> {
    let Some(decider) = decider else {
        return Ok(());
    };
    match decider.evaluate(host, tool) {
        Decision::Allow => Ok(()),
        Decision::Deny => Err(WebError::new(
            WebErrorKind::PolicyDenied,
            format!("network call to '{host}' blocked by network policy"),
        )),
        Decision::Prompt => Ok(()),
    }
}

/// Race a web operation against its cancellation token. This is the seam's
/// single cancellation point: every transport the service performs is
/// aborted (future dropped) the moment the token fires.
async fn race_cancel<F, T>(cancel: Option<&CancellationToken>, fut: F) -> Result<T, WebError>
where
    F: std::future::Future<Output = Result<T, WebError>>,
{
    match cancel {
        Some(token) => {
            tokio::select! {
                biased;
                () = token.cancelled() => Err(WebError::new(
                    WebErrorKind::Cancelled,
                    "web request cancelled",
                )),
                result = fut => result,
            }
        }
        None => fut.await,
    }
}

fn search_client(
    timeout: Duration,
    connect_timeout: Option<Duration>,
    user_agent: Option<&'static str>,
) -> Result<reqwest::Client, WebError> {
    let mut builder = reqwest::Client::builder().timeout(timeout);
    if let Some(ct) = connect_timeout {
        builder = builder.connect_timeout(ct);
    }
    if let Some(ua) = user_agent {
        builder = builder.user_agent(ua);
    }
    builder.build().map_err(|e| {
        WebError::new(
            WebErrorKind::Transport,
            format!("Failed to build HTTP client: {e}"),
        )
    })
}

fn reqwest_web_error(context: &str, e: &reqwest::Error) -> WebError {
    let kind = if e.is_timeout() {
        WebErrorKind::Timeout
    } else {
        WebErrorKind::Transport
    };
    WebError::new(kind, format!("{context}: {e}"))
}

async fn read_body(
    context: &str,
    resp: reqwest::Response,
) -> Result<(reqwest::StatusCode, String), WebError> {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| WebError::new(WebErrorKind::Body, format!("{context}: {e}")))?;
    Ok((status, body))
}

async fn fetch_outcome_from(
    resp: reqwest::Response,
    max_bytes: u64,
) -> Result<FetchOutcome, WebError> {
    let url = resp.url().to_string();
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let headers = response_headers(resp.headers());
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| WebError::new(WebErrorKind::Body, format!("failed to read body: {e}")))?;
    let total_bytes = bytes.len() as u64;
    let truncated = total_bytes > max_bytes;
    let keep = max_bytes.min(total_bytes) as usize;
    Ok(FetchOutcome {
        url,
        status: status.as_u16(),
        headers,
        content_type,
        body: bytes[..keep].to_vec(),
        truncated,
    })
}

// === Fetch target validation (SSRF + DNS pinning) ==========================

/// Check if an IP address is loopback, private, link-local, cloud-metadata,
/// multicast, or reserved — all addresses that should not be reachable via
/// an LLM-initiated fetch (SSRF prevention).
fn is_restricted_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_unspecified()
                // 100.64.0.0/10 — Carrier-grade NAT (CGNAT / shared address space)
                || matches!(v4.octets(), [100, 64..=127, ..])
                // 169.254.169.254 — cloud metadata (AWS/GCP/Azure)
                || *ip == IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))
                // 198.18.0.0/15 — IETF benchmark testing
                || matches!(v4.octets(), [198, 18..=19, ..])
                // 240.0.0.0/4 — reserved (former Class E)
                || v4.octets()[0] >= 240
        }
        IpAddr::V6(v6) => {
            // IPv4-mapped IPv6 addresses (::ffff:a.b.c.d) — unwrap and check as IPv4
            // to prevent bypass via ::ffff:127.0.0.1 etc.
            if v6.is_unspecified()
                || matches!(v6.octets(), [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, ..])
            {
                return true;
            }
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_restricted_ip(&IpAddr::V4(v4));
            }
            v6.is_loopback()
                || v6.is_multicast()
                || matches!(v6.segments(), [0xfc00..=0xfdff, ..]) // ULA fc00::/7
                || matches!(v6.segments(), [0xfe80..=0xfebf, ..]) // Link-local fe80::/10
        }
    }
}

/// Validate one fetch hop: network policy, then the SSRF block, resolving
/// DNS ahead of the request so the validated IP can be pinned (prevents
/// TOCTOU rebinding). Returns the `(hostname, ip)` pin when applicable.
async fn validate_fetch_target(
    url: &reqwest::Url,
    req: &FetchRequest,
) -> Result<Option<(String, IpAddr)>, WebError> {
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(WebError::new(
            WebErrorKind::InvalidUrl,
            "only http:// and https:// URLs are supported",
        ));
    }

    let host = url
        .host_str()
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| WebError::new(WebErrorKind::InvalidUrl, "URL must include a host"))?;

    check_policy(req.policy.as_ref(), &host, req.tool)?;

    // SSRF protection: reject loopback names and private/link-local/loopback
    // IPs. Prevents LLM-prompted requests to cloud metadata
    // (169.254.169.254), localhost services, and internal networks.
    if host == "localhost" || host == "localhost.localdomain" {
        return Err(WebError::new(
            WebErrorKind::PolicyDenied,
            "requests to localhost are not allowed",
        ));
    }
    // Normalize bracketed IPv6 literals before the literal-IP check so they
    // route through the same restricted-IP policy as unbracketed forms
    // (GHSA-88gh-2526-gfrr).
    let ip_candidate = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host.as_str());
    if let Ok(ip) = ip_candidate.parse::<IpAddr>() {
        if is_restricted_ip(&ip) {
            return Err(WebError::new(
                WebErrorKind::PolicyDenied,
                format!("IP {ip} is a restricted address (private/loopback/link-local)"),
            ));
        }
        return Ok(None);
    }

    let mut first_valid: Option<IpAddr> = None;
    if let Ok(addrs) = tokio::net::lookup_host((host.as_str(), 0u16)).await {
        for addr in addrs {
            validate_dns_resolved_ip(&host, &addr.ip(), req.policy.as_ref(), req.tool)?;
            if first_valid.is_none() {
                first_valid = Some(addr.ip());
            }
        }
    }

    // If DNS resolution fails, let the HTTP request proceed and fail naturally.
    Ok(first_valid.map(|validated_ip| (host, validated_ip)))
}

fn validate_dns_resolved_ip(
    host: &str,
    ip: &IpAddr,
    decider: Option<&NetworkPolicyDecider>,
    tool: &str,
) -> Result<(), WebError> {
    if !is_restricted_ip(ip) {
        return Ok(());
    }

    // Allow the resolved IP past the restricted-IP block if either:
    //   * it falls inside a configured fake-IP placeholder range (a TUN /
    //     transparent-proxy setup in `fake-ip` mode resolves every host into a
    //     reserved range such as `198.18.0.0/15`), or
    //   * the host is on the explicitly-trusted proxy list.
    // Real private/loopback/link-local/metadata IPs match neither and stay blocked.
    if let Some(decider) = decider
        && (decider.is_trusted_fakeip_addr(ip) || decider.trusts_proxy_fakeip_host(host))
    {
        decider.record_trusted_proxy_fakeip_allow(host, tool);
        return Ok(());
    }

    Err(WebError::new(
        WebErrorKind::PolicyDenied,
        format!("resolved IP {ip} is a restricted address (private/loopback/link-local)"),
    ))
}

/// Response-header names that never reach the model-visible transcript:
/// session and auth material a fetched server put on the connection has no
/// diagnostic value and would leak credentials into session logs.
const REDACTED_RESPONSE_HEADERS: &[&str] = &[
    "set-cookie",
    "set-cookie2",
    "cookie",
    "authorization",
    "proxy-authorization",
    "www-authenticate",
];

fn response_headers(headers: &reqwest::header::HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            let name = name.as_str().to_ascii_lowercase();
            if REDACTED_RESPONSE_HEADERS.contains(&name.as_str()) {
                return None;
            }
            value.to_str().ok().map(|value| (name, value.to_string()))
        })
        .collect()
}

// === JSON result-shape parsing (API providers) =============================

/// Extract `SearchHit`s from a provider JSON array using per-field alias
/// lists. Shared by the API-backed providers so each keeps its original
/// field-tolerant behavior.
fn parse_generic_results(
    node: &Value,
    field_aliases: &[&[&str]],
    max_results: usize,
) -> Vec<SearchHit> {
    fn pick<'a>(item: &'a Value, aliases: &[&str]) -> Option<&'a str> {
        for alias in aliases {
            if let Some(s) = item.get(*alias).and_then(Value::as_str) {
                return Some(s);
            }
        }
        None
    }

    let Some(title_aliases) = field_aliases.first().copied() else {
        return Vec::new();
    };
    let Some(url_aliases) = field_aliases.get(1).copied() else {
        return Vec::new();
    };
    let Some(snippet_aliases) = field_aliases.get(2).copied() else {
        return Vec::new();
    };

    node.as_array()
        .into_iter()
        .flat_map(|arr| arr.iter())
        .filter_map(|item| {
            let title = pick(item, title_aliases)?.trim();
            let url = pick(item, url_aliases)?.trim();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            let snippet = pick(item, snippet_aliases)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
            Some(SearchHit {
                title: title.to_string(),
                url: url.to_string(),
                snippet,
            })
        })
        .take(max_results)
        .collect()
}

fn baidu_error_message(parsed: &Value) -> Option<String> {
    let code = parsed
        .get("error_code")
        .or_else(|| parsed.get("code"))
        .and_then(|v| v.as_i64())?;
    if code == 0 {
        return None;
    }
    let message = parsed
        .get("error_msg")
        .or_else(|| parsed.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown error");
    Some(format!("Baidu search API error (code {code}: {message})"))
}

fn baidu_search_payload(query: &str, max_results: usize) -> Value {
    json!({
        "messages": [
            {
                "role": "user",
                "content": query,
            }
        ],
        "search_source": "baidu_search_v2",
        "resource_type_filter": [
            {
                "type": "web",
                "top_k": max_results,
            }
        ],
    })
}

fn volcengine_search_payload(query: &str, max_results: usize) -> Value {
    json!({
        "model": "doubao-seed-2-0-lite-260428",
        "stream": false,
        "tools": [{"type": "web_search"}],
        "input": [{
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": format!(
                    "Search the web for: {query}\n\n\
                     CRITICAL: Respond ONLY with a valid JSON object. No markdown, no explanation.\n\
                     Schema: {{\"results\":[{{\"title\":\"...\",\"url\":\"https://...\",\"snippet\":\"...\"}}]}}\n\
                     - results: 1-{max_results} most relevant pages\n\
                     - title: page title (required)\n\
                     - url: full URL starting with https:// (required)\n\
                     - snippet: 1-2 sentence factual summary (required)\n\
                     - If zero results: {{\"results\":[]}}\n\
                     - Your entire response must be valid, parseable JSON."
                )
            }]
        }]
    })
}

/// Extracts the model's text response from a Volcengine Responses API output.
fn volcengine_extract_text(parsed: &Value) -> Option<String> {
    parsed
        .get("output")
        .and_then(|v| v.as_array())
        .into_iter()
        .flat_map(|arr| arr.iter().rev())
        .find(|item| item.get("type").and_then(|t| t.as_str()) == Some("message"))
        .and_then(|msg| msg.get("content").and_then(|c| c.as_array()))
        .and_then(|content| {
            content
                .iter()
                .find(|c| c.get("text").and_then(|t| t.as_str()).is_some())
        })
        .and_then(|c| c.get("text").and_then(|t| t.as_str()))
        .map(|s| s.to_string())
}

/// Checks for business-logic errors in a Volcengine Responses API response.
fn volcengine_error_message(parsed: &Value) -> Option<String> {
    let error = parsed.get("error")?;
    let code = error
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let message = error
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("no details");
    Some(format!("Volcengine API error (code {code}: {message})"))
}

/// Parses Volcengine model-generated JSON results into `SearchHit` items.
fn parse_volcengine_results(response_text: &str, max_results: usize) -> Vec<SearchHit> {
    let json_text = extract_json_block(response_text).unwrap_or(response_text);

    let parsed: Value = match serde_json::from_str(json_text) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    parsed
        .get("results")
        .and_then(|v| v.as_array())
        .into_iter()
        .flat_map(|arr| arr.iter())
        .filter_map(|item| {
            let title = item.get("title").and_then(|s| s.as_str())?.trim();
            let url = item.get("url").and_then(|s| s.as_str())?.trim();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            let snippet = item
                .get("snippet")
                .and_then(|s| s.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
            Some(SearchHit {
                title: title.to_string(),
                url: url.to_string(),
                snippet,
            })
        })
        .take(max_results)
        .collect()
}

/// Attempts to extract a JSON block from text that may be wrapped in
/// markdown fences (```json ... ```) or contain surrounding commentary.
fn extract_json_block(text: &str) -> Option<&str> {
    if let Some(start) = text.find("```json") {
        let inner = &text[start + 7..];
        if let Some(end) = inner.find("```") {
            return Some(inner[..end].trim());
        }
    }
    if let Some(start) = text.find('{')
        && let Some(end) = text.rfind('}')
    {
        return Some(&text[start..=end]);
    }
    None
}

// === Provider-native result parsing =======================================

/// Search results from a provider-native response, in either wire shape:
/// Anthropic returns objects with `title`/`url` (plus `citations` entries on
/// the answer blocks); BigModel returns one `tool_result` whose `content` is
/// a stringified list of dicts (`{'title': …, 'link': …, 'content': …}`).
fn parse_provider_native_results(parsed: &Value, max_results: usize) -> Vec<SearchHit> {
    let mut hits: Vec<SearchHit> = Vec::new();
    collect_native_hits(parsed, &mut hits, max_results);
    hits.truncate(max_results);
    hits
}

fn collect_native_hits(value: &Value, hits: &mut Vec<SearchHit>, max_results: usize) {
    if hits.len() >= max_results {
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                collect_native_hits(item, hits, max_results);
            }
        }
        Value::Object(map) => {
            let title = map.get("title").and_then(Value::as_str);
            let url = map
                .get("url")
                .or_else(|| map.get("link"))
                .and_then(Value::as_str);
            if let (Some(title), Some(url)) = (title, url)
                && is_http_url(url)
            {
                let snippet = map
                    .get("content")
                    .or_else(|| map.get("snippet"))
                    .and_then(Value::as_str);
                push_native_hit(hits, title, url, snippet);
                return;
            }
            for value in map.values() {
                collect_native_hits(value, hits, max_results);
            }
        }
        Value::String(text) => scan_stringified_hits(text, hits, max_results),
        _ => {}
    }
}

/// Read `title`/`link`/`content` triples out of a stringified list of dicts.
/// Records are ordered title → link → content, so the title is the last one
/// before the link and the snippet is the first one after it.
fn scan_stringified_hits(text: &str, hits: &mut Vec<SearchHit>, max_results: usize) {
    let mut cursor = 0;
    while hits.len() < max_results {
        let Some((link_at, link)) = find_quoted_field(text, "link", cursor) else {
            break;
        };
        cursor = link_at + 1;
        if !is_http_url(&link) {
            continue;
        }
        let title = last_quoted_field_before(text, "title", link_at).unwrap_or_default();
        let snippet = find_quoted_field(text, "content", link_at + 1).map(|(_, value)| value);
        push_native_hit(hits, &title, &link, snippet.as_deref());
    }
}

fn push_native_hit(hits: &mut Vec<SearchHit>, title: &str, url: &str, snippet: Option<&str>) {
    let url = url.trim();
    if url.is_empty() || hits.iter().any(|hit| hit.url == url) {
        return;
    }
    let title = title.trim();
    hits.push(SearchHit {
        title: if title.is_empty() {
            url.to_string()
        } else {
            title.to_string()
        },
        url: url.to_string(),
        snippet: snippet
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
    });
}

fn is_http_url(value: &str) -> bool {
    let value = value.trim();
    value.starts_with("http://") || value.starts_with("https://")
}

/// Find `'key': 'value'` (or the double-quoted JSON form) at or after `from`,
/// returning the key's byte offset and the unescaped value. Both quote styles
/// occur: Python's repr switches quotes when the value contains the other one.
fn find_quoted_field(hay: &str, key: &str, from: usize) -> Option<(usize, String)> {
    let bytes = hay.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        let quote = bytes[i];
        if (quote == b'\'' || quote == b'"')
            && bytes.get(i + 1..i + 1 + key.len()) == Some(key.as_bytes())
            && bytes.get(i + 1 + key.len()) == Some(&quote)
        {
            let mut j = i + 2 + key.len();
            while matches!(bytes.get(j), Some(b' ' | b'\t' | b'\r' | b'\n')) {
                j += 1;
            }
            if bytes.get(j) == Some(&b':') {
                j += 1;
                while matches!(bytes.get(j), Some(b' ' | b'\t' | b'\r' | b'\n')) {
                    j += 1;
                }
                let value_quote = *bytes.get(j)?;
                if value_quote == b'\'' || value_quote == b'"' {
                    j += 1;
                    let mut value = String::new();
                    let mut segment_start = j;
                    while j < bytes.len() {
                        if bytes[j] == b'\\' && bytes.get(j + 1).is_some_and(|b| b.is_ascii()) {
                            value.push_str(&hay[segment_start..j]);
                            match bytes[j + 1] {
                                b'n' => value.push('\n'),
                                b't' => value.push('\t'),
                                b'r' => value.push('\r'),
                                other => value.push(char::from(other)),
                            }
                            j += 2;
                            segment_start = j;
                        } else if bytes[j] == value_quote {
                            value.push_str(&hay[segment_start..j]);
                            return Some((i, value));
                        } else {
                            j += 1;
                        }
                    }
                }
            }
        }
        i += 1;
    }
    None
}

fn last_quoted_field_before(hay: &str, key: &str, limit: usize) -> Option<String> {
    let mut found = None;
    let mut from = 0;
    while let Some((at, value)) = find_quoted_field(hay, key, from) {
        if at >= limit {
            break;
        }
        found = Some(value);
        from = at + 1;
    }
    found
}

/// `https://host/api/anthropic` → `…/api/anthropic/v1/messages`. A base that
/// already ends in `/v1` (or the full path) is not doubled.
fn messages_endpoint(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/v1/messages") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/messages")
    } else {
        format!("{base}/v1/messages")
    }
}

/// Anthropic errors are `{"type":"error","error":{"type","message"}}`;
/// BigModel adds `{"error":{"code","message"}}` business errors on top.
fn provider_native_error_message(parsed: &Value) -> Option<String> {
    let error = parsed.get("error")?;
    let message = error.get("message").and_then(Value::as_str)?;
    if message.trim().is_empty() {
        return None;
    }
    let code = ["type", "code"].iter().find_map(|key| {
        let value = error.get(*key)?;
        value
            .as_str()
            .map(str::to_string)
            .or_else(|| value.as_i64().map(|number| number.to_string()))
    });
    Some(match code {
        Some(code) => format!("Provider-native web search error ({code}): {message}"),
        None => format!("Provider-native web search error: {message}"),
    })
}

// === SERP HTML scrape parsing ==============================================

// Cached regex patterns for HTML parsing
static TITLE_RE: OnceLock<Regex> = OnceLock::new();
static SNIPPET_RE: OnceLock<Regex> = OnceLock::new();
static TAG_RE: OnceLock<Regex> = OnceLock::new();
static BING_RESULT_RE: OnceLock<Regex> = OnceLock::new();
static BING_TITLE_RE: OnceLock<Regex> = OnceLock::new();
static BING_SNIPPET_RE: OnceLock<Regex> = OnceLock::new();
static BEARER_TOKEN_RE: OnceLock<Regex> = OnceLock::new();

fn get_title_re() -> &'static Regex {
    TITLE_RE.get_or_init(|| {
        Regex::new(r#"<a[^>]*class=\"result__a\"[^>]*href=\"([^\"]+)\"[^>]*>(.*?)</a>"#)
            .expect("title regex pattern is valid")
    })
}

fn get_snippet_re() -> &'static Regex {
    SNIPPET_RE.get_or_init(|| {
        Regex::new(
            r#"<a[^>]*class=\"result__snippet\"[^>]*>(.*?)</a>|<div[^>]*class=\"result__snippet\"[^>]*>(.*?)</div>"#,
        )
        .expect("snippet regex pattern is valid")
    })
}

fn get_tag_re() -> &'static Regex {
    TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").expect("tag regex pattern is valid"))
}

fn get_bing_result_re() -> &'static Regex {
    BING_RESULT_RE.get_or_init(|| {
        Regex::new(r#"(?is)<li[^>]*class=\"[^\"]*\bb_algo\b[^\"]*\"[^>]*>(.*?)</li>"#)
            .expect("bing result regex pattern is valid")
    })
}

fn get_bing_title_re() -> &'static Regex {
    BING_TITLE_RE.get_or_init(|| {
        Regex::new(r#"(?is)<h2[^>]*>.*?<a[^>]*href=\"([^\"]+)\"[^>]*>(.*?)</a>"#)
            .expect("bing title regex pattern is valid")
    })
}

fn get_bing_snippet_re() -> &'static Regex {
    BING_SNIPPET_RE.get_or_init(|| {
        Regex::new(r#"(?is)<div[^>]*class=\"[^\"]*\bb_caption\b[^\"]*\"[^>]*>.*?<p[^>]*>(.*?)</p>"#)
            .expect("bing snippet regex pattern is valid")
    })
}

fn get_bearer_token_re() -> &'static Regex {
    BEARER_TOKEN_RE.get_or_init(|| {
        Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]+")
            .expect("bearer token regex pattern is valid")
    })
}

async fn run_bing_search(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Result<Vec<SearchHit>, WebError> {
    let encoded = url_encode(query);
    let url = format!("https://www.bing.com/search?q={encoded}");
    let resp = client
        .get(&url)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "en-US,en;q=0.9")
        .send()
        .await
        .map_err(|e| reqwest_web_error("Bing search request failed", &e))?;
    let (status, body) = read_body("Failed to read Bing search response", resp).await?;
    if !status.is_success() {
        return Err(WebError::new(
            WebErrorKind::Status(status.as_u16()),
            format!("Bing search failed: HTTP {}", status.as_u16()),
        ));
    }

    Ok(parse_bing_results(&body, max_results))
}

fn parse_duckduckgo_results(html: &str, max_results: usize) -> Vec<SearchHit> {
    let title_re = get_title_re();
    let snippet_re = get_snippet_re();
    let snippets: Vec<String> = snippet_re
        .captures_iter(html)
        .filter_map(|cap| cap.get(1).or_else(|| cap.get(2)))
        .map(|m| normalize_text(m.as_str()))
        .collect();

    let mut results = Vec::new();
    for (idx, cap) in title_re.captures_iter(html).enumerate() {
        if results.len() >= max_results {
            break;
        }
        let href = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let title_raw = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let title = normalize_text(title_raw);
        if title.is_empty() {
            continue;
        }
        let url = normalize_url(href);
        let snippet = snippets
            .get(idx)
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

        results.push(SearchHit {
            title,
            url,
            snippet,
        });
    }

    if is_likely_spam_results(&results) {
        // Same defence as the Bing path (#964): a DDG fallback page can
        // also serve a single-domain stuffed result set when the upstream
        // is degraded. Drop rather than mislead the model.
        return Vec::new();
    }
    results
}

fn is_duckduckgo_challenge(html: &str) -> bool {
    html.contains("anomaly-modal") || html.contains("Unfortunately, bots use DuckDuckGo too")
}

fn parse_bing_results(html: &str, max_results: usize) -> Vec<SearchHit> {
    let mut results = Vec::new();
    for cap in get_bing_result_re().captures_iter(html) {
        if results.len() >= max_results {
            break;
        }
        let Some(block) = cap.get(1).map(|m| m.as_str()) else {
            continue;
        };
        let Some(title_cap) = get_bing_title_re().captures(block) else {
            continue;
        };
        let href = title_cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let title_raw = title_cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let title = normalize_text(title_raw);
        if title.is_empty() {
            continue;
        }
        let snippet = get_bing_snippet_re()
            .captures(block)
            .and_then(|snippet_cap| snippet_cap.get(1))
            .map(|m| normalize_text(m.as_str()))
            .filter(|s| !s.is_empty());

        results.push(SearchHit {
            title,
            url: normalize_bing_url(href),
            snippet,
        });
    }

    if is_likely_spam_results(&results) {
        // Bing's scraping endpoint occasionally serves a stuffed page
        // where the same low-quality domain owns most of the b_algo
        // entries — #964 reported eight in a row from
        // `astralia.forumgratuit.org` for unrelated queries. Treat the
        // batch as "no results" so the caller surfaces a clean failure
        // message instead of routing the model toward junk.
        return Vec::new();
    }
    results
}

/// Heuristic spam detector for scraped SERP HTML (#964).
///
/// Returns `true` when one root domain owns at least 60% of the result
/// set and there are at least three results. A real-world top-five page
/// from Google/Bing/DDG mixes domains; a result page dominated by one
/// host is almost always SEO spam or a bot-detection-stuffed substitute.
fn is_likely_spam_results(results: &[SearchHit]) -> bool {
    if results.len() < 3 {
        return false;
    }
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for r in results {
        if let Some(host) = root_domain(&r.url) {
            *counts.entry(host).or_insert(0) += 1;
        }
    }
    let Some(&max) = counts.values().max() else {
        return false;
    };
    // 60% threshold: 3-of-5, 4-of-6, 5-of-8 all trip; 2-of-5 doesn't.
    max * 5 >= results.len() * 3
}

/// Extract the registrable root domain (eTLD+1 approximation) from a URL
/// so spam detection groups `astralia.forumgratuit.org` with
/// `russia.forumgratuit.org`. Returns lowercase host minus the leftmost
/// label, or the bare host when there are only two labels.
fn root_domain(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = after_scheme.split(['/', '?', '#']).next()?;
    let host = host.split('@').next_back()?;
    let host = host.split(':').next()?.to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    let labels: Vec<&str> = host.split('.').filter(|s| !s.is_empty()).collect();
    if labels.len() <= 2 {
        return Some(host);
    }
    Some(labels[labels.len().saturating_sub(2)..].join("."))
}

fn normalize_url(href: &str) -> String {
    if let Some(uddg) = extract_query_param(href, "uddg") {
        let decoded = percent_decode(&uddg);
        if !decoded.is_empty() {
            return decoded;
        }
    }
    if href.starts_with("//") {
        return format!("https:{href}");
    }
    if href.starts_with('/') {
        return format!("https://duckduckgo.com{href}");
    }
    href.to_string()
}

fn normalize_bing_url(href: &str) -> String {
    // Bing wraps every SERP result URL in a `/ck/a?...&u=<base64>` click-tracking
    // redirect, and in the raw HTML the separators are `&amp;` entities. Without
    // decoding entities first, `extract_query_param` looks for `u` but the actual
    // key is `amp;u`, so the real URL is never recovered: every result collapses to
    // a `bing.com` root domain, which the spam heuristic then rejects — yielding
    // zero results for the default Bing backend. Decode entities before parsing.
    let href = decode_html_entities(href);
    let href = href.as_str();
    if let Some(encoded) = extract_query_param(href, "u") {
        let decoded = percent_decode(&encoded);
        let token = decoded.strip_prefix("a1").unwrap_or(&decoded);
        let mut padded = token.replace('-', "+").replace('_', "/");
        while !padded.len().is_multiple_of(4) {
            padded.push('=');
        }
        if let Ok(bytes) = general_purpose::STANDARD.decode(padded)
            && let Ok(url) = String::from_utf8(bytes)
            && (url.starts_with("http://") || url.starts_with("https://"))
        {
            return url;
        }
    }
    if href.starts_with("//") {
        return format!("https:{href}");
    }
    if href.starts_with('/') {
        return format!("https://www.bing.com{href}");
    }
    href.to_string()
}

fn normalize_text(text: &str) -> String {
    let stripped = strip_html_tags(text);
    let decoded = decode_html_entities(&stripped);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_html_tags(text: &str) -> String {
    get_tag_re().replace_all(text, "").to_string()
}

fn decode_html_entities(text: &str) -> String {
    static ENTITY_RE: OnceLock<Regex> = OnceLock::new();
    let re = ENTITY_RE.get_or_init(|| {
        Regex::new(r"&(?:#(\d+)|#x([0-9A-Fa-f]+)|([a-zA-Z]+));").expect("HTML entity regex")
    });

    re.replace_all(text, |caps: &regex::Captures| {
        if let Some(dec) = caps.get(1) {
            return dec
                .as_str()
                .parse::<u32>()
                .ok()
                .and_then(std::char::from_u32)
                .unwrap_or('\u{FFFD}')
                .to_string();
        }
        if let Some(hex) = caps.get(2) {
            return u32::from_str_radix(hex.as_str(), 16)
                .ok()
                .and_then(std::char::from_u32)
                .unwrap_or('\u{FFFD}')
                .to_string();
        }
        let named = caps.get(3).map(|m| m.as_str());
        match named {
            Some("amp") => "&",
            Some("lt") => "<",
            Some("gt") => ">",
            Some("quot") => "\"",
            Some("apos") => "'",
            Some("nbsp") => " ",
            Some("copy") => "\u{00A9}",
            Some("reg") => "\u{00AE}",
            Some("mdash") => "\u{2014}",
            Some("ndash") => "\u{2013}",
            Some("lsquo") => "\u{2018}",
            Some("rsquo") => "\u{2019}",
            Some("ldquo") => "\u{201C}",
            Some("rdquo") => "\u{201D}",
            Some("hellip") => "\u{2026}",
            _ => return caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string(),
        }
        .to_string()
    })
    .to_string()
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = &input[i + 1..i + 3];
                if let Ok(val) = u8::from_str_radix(hex, 16) {
                    out.push(val);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
            }
            b'+' => out.push(b' '),
            _ => out.push(bytes[i]),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn extract_query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    for part in query.split('&') {
        let mut iter = part.splitn(2, '=');
        let name = iter.next().unwrap_or("");
        if name == key {
            return iter.next().map(str::to_string);
        }
    }
    None
}

fn truncate_error_body(body: &str) -> String {
    let stripped = sanitize_error_body(body);
    if stripped.len() <= ERROR_BODY_PREVIEW_BYTES {
        stripped
    } else {
        let mut end = ERROR_BODY_PREVIEW_BYTES;
        while !stripped.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &stripped[..end])
    }
}

fn sanitize_error_body(body: &str) -> String {
    let stripped = strip_html_tags(body);
    let visible: String = stripped
        .chars()
        .filter(|c| !c.is_control() || c.is_ascii_whitespace())
        .collect();
    get_bearer_token_re()
        .replace_all(&visible, "Bearer [REDACTED]")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(url: &str) -> SearchHit {
        SearchHit {
            title: "x".into(),
            url: url.into(),
            snippet: None,
        }
    }

    // === provider host reporting ==========================================

    #[test]
    fn search_hosts_follow_the_configured_provider() {
        let hosts_for =
            |provider: SearchProvider| HttpWebService::new(provider, None).search_hosts();
        assert_eq!(
            hosts_for(SearchProvider::Tavily),
            vec!["api.tavily.com".to_string()]
        );
        assert_eq!(
            hosts_for(SearchProvider::Bing),
            vec![
                "www.bing.com".to_string(),
                "html.duckduckgo.com".to_string()
            ]
        );
        assert_eq!(
            hosts_for(SearchProvider::DuckDuckGo),
            vec!["html.duckduckgo.com".to_string()]
        );
    }

    // === missing-key failures stay loud (no silent provider fallback) =====

    #[tokio::test]
    async fn tavily_provider_without_api_key_surfaces_config_error() {
        // Trust-boundary pin: if a user has opted into Tavily but forgot the
        // api_key, the service must NOT silently fall through to
        // DuckDuckGo (which would expose the query to a different provider
        // than the user authorised).
        let svc = HttpWebService::new(SearchProvider::Tavily, None);
        let err = svc
            .search(SearchRequest::new("anything"))
            .await
            .expect_err("missing api_key must surface");
        assert_eq!(err.kind(), WebErrorKind::Config);
        let msg = err.to_string();
        assert!(
            msg.contains("Tavily") && msg.contains("API key"),
            "error must name the provider and missing key; got `{msg}`"
        );
    }

    #[tokio::test]
    async fn bocha_provider_without_api_key_surfaces_config_error() {
        let svc = HttpWebService::new(SearchProvider::Bocha, None);
        let err = svc
            .search(SearchRequest::new("anything"))
            .await
            .expect_err("missing api_key must surface");
        assert_eq!(err.kind(), WebErrorKind::Config);
        let msg = err.to_string();
        assert!(msg.contains("Bocha") && msg.contains("API key"));
    }

    #[tokio::test]
    // The env mutex serializes env mutation across the test binary; every
    // other holder acquires it only briefly (no blocking), so holding the
    // guard across this await cannot deadlock.
    #[allow(clippy::await_holding_lock)]
    async fn baidu_provider_without_api_key_surfaces_config_error() {
        let _guard = codesmith_agent_runtime::test_support::lock_test_env();
        let _env =
            codesmith_agent_runtime::test_support::EnvVarGuard::remove("BAIDU_SEARCH_API_KEY");
        let svc = HttpWebService::new(SearchProvider::Baidu, None);
        let err = svc
            .search(SearchRequest::new("anything"))
            .await
            .expect_err("missing api_key must surface");
        assert_eq!(err.kind(), WebErrorKind::Config);
        assert!(err.to_string().contains("Baidu"));
    }

    #[tokio::test]
    // The env mutex serializes env mutation across the test binary; every
    // other holder acquires it only briefly (no blocking), so holding the
    // guard across this await cannot deadlock.
    #[allow(clippy::await_holding_lock)]
    async fn volcengine_provider_without_api_key_lists_supported_env_fallbacks() {
        let _guard = codesmith_agent_runtime::test_support::lock_test_env();
        let _volc =
            codesmith_agent_runtime::test_support::EnvVarGuard::remove("VOLCENGINE_API_KEY");
        let _volc_ark =
            codesmith_agent_runtime::test_support::EnvVarGuard::remove("VOLCENGINE_ARK_API_KEY");
        let _ark = codesmith_agent_runtime::test_support::EnvVarGuard::remove("ARK_API_KEY");
        let svc = HttpWebService::new(SearchProvider::Volcengine, None);
        let err = svc
            .search(SearchRequest::new("anything"))
            .await
            .expect_err("missing api_key must surface");
        assert_eq!(err.kind(), WebErrorKind::Config);
        let msg = err.to_string();
        assert!(msg.contains("Volcengine") && msg.contains("API key"));
        assert!(msg.contains("VOLCENGINE_API_KEY"));
        assert!(msg.contains("VOLCENGINE_ARK_API_KEY"));
        assert!(msg.contains("ARK_API_KEY"));
        assert!(!msg.contains("DEEPSEEK_SEARCH_API_KEY"));
    }

    #[tokio::test]
    // The env mutex serializes env mutation across the test binary; every
    // other holder acquires it only briefly (no blocking), so holding the
    // guard across this await cannot deadlock.
    #[allow(clippy::await_holding_lock)]
    async fn metaso_provider_requires_a_configured_key() {
        // The built-in community default key was removed: with neither a
        // config key nor the env var set, the request must fail loud before
        // any network I/O. (Guard the env so a developer machine exporting
        // METASO_API_KEY cannot flip the result.)
        let _guard = codesmith_agent_runtime::test_support::lock_test_env();
        let _metaso = codesmith_agent_runtime::test_support::EnvVarGuard::remove("METASO_API_KEY");
        let svc = HttpWebService::new(SearchProvider::Metaso, None);
        let err = svc
            .search(SearchRequest::new("anything"))
            .await
            .expect_err("no key configured must fail, not hit the network");
        assert_eq!(err.kind(), WebErrorKind::Config);
        assert!(
            err.to_string().contains("requires an API key"),
            "got `{err}`"
        );
    }

    // === cancellation =====================================================

    #[tokio::test]
    async fn cancelled_token_aborts_the_search_with_cancelled_error() {
        let svc = HttpWebService::default();
        let token = CancellationToken::new();
        token.cancel();
        let err = svc
            .search(SearchRequest::new("q").with_cancel(Some(token)))
            .await
            .expect_err("pre-cancelled token must abort");
        assert_eq!(err.kind(), WebErrorKind::Cancelled);
    }

    #[tokio::test]
    async fn cancelled_token_aborts_the_fetch_with_cancelled_error() {
        let svc = HttpWebService::default();
        let token = CancellationToken::new();
        token.cancel();
        let err = svc
            .fetch(FetchRequest::new("https://example.com/").with_cancel(Some(token)))
            .await
            .expect_err("pre-cancelled token must abort");
        assert_eq!(err.kind(), WebErrorKind::Cancelled);
    }

    // === policy + SSRF (fetch) ============================================

    fn denying_decider() -> NetworkPolicyDecider {
        NetworkPolicyDecider::new(
            codesmith_agent_runtime::network_policy::NetworkPolicy {
                default: Decision::Deny.into(),
                allow: vec!["api.deepseek.com".to_string()],
                deny: vec![],
                proxy: Vec::new(),
                audit: false,
            },
            None,
        )
    }

    fn prompt_decider(allow: &[&str], deny: &[&str]) -> NetworkPolicyDecider {
        NetworkPolicyDecider::new(
            codesmith_agent_runtime::network_policy::NetworkPolicy {
                default: Decision::Prompt.into(),
                allow: allow.iter().map(|s| (*s).to_string()).collect(),
                deny: deny.iter().map(|s| (*s).to_string()).collect(),
                proxy: Vec::new(),
                audit: false,
            },
            None,
        )
    }

    #[tokio::test]
    async fn fetch_policy_deny_is_a_policy_error() {
        let svc = HttpWebService::default();
        let err = svc
            .fetch(
                FetchRequest::new("https://example.com/foo")
                    .with_policy(Some(denying_decider()))
                    .for_tool("fetch_url"),
            )
            .await
            .expect_err("blocked host must fail");
        assert_eq!(err.kind(), WebErrorKind::PolicyDenied);
        assert!(err.to_string().contains("blocked by network policy"));
    }

    #[test]
    fn fetch_policy_prompt_passes_after_gate_semantics() {
        // P1-5: a Prompt decision must not fail at execute time — the inline
        // approval gate owns the ask before the service ever runs. Deny
        // still hard-fails inside the service.
        let prompt = prompt_decider(&[], &[]);
        check_policy(Some(&prompt), "unlisted.example.com", "fetch_url")
            .expect("Prompt must not fail at execute time");
        let denied = prompt_decider(&[], &["evil.example.com"]);
        let err = check_policy(Some(&denied), "evil.example.com", "fetch_url")
            .expect_err("Deny must hard-fail");
        assert_eq!(err.kind(), WebErrorKind::PolicyDenied);
    }

    #[tokio::test]
    async fn fetch_rejects_localhost_and_restricted_literals() {
        let svc = HttpWebService::default();
        for url in [
            "http://localhost:8080/admin",
            "http://127.0.0.1/x",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/",
            "http://[fc00::1]/",
            "http://[fe80::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[::]/",
            "http://198.18.0.1/status",
        ] {
            let err = svc
                .fetch(FetchRequest::new(url))
                .await
                .expect_err("{url} must be rejected");
            assert_eq!(
                err.kind(),
                WebErrorKind::PolicyDenied,
                "{url}: {}",
                err.message
            );
        }
    }

    #[tokio::test]
    async fn fetch_rejects_non_http_schemes() {
        let svc = HttpWebService::default();
        let err = svc
            .fetch(FetchRequest::new("file:///etc/passwd"))
            .await
            .expect_err("file scheme must fail");
        assert_eq!(err.kind(), WebErrorKind::InvalidUrl);
    }

    #[test]
    fn restricted_ip_classification() {
        assert!(is_restricted_ip(&"127.0.0.1".parse().unwrap()));
        assert!(is_restricted_ip(&"::1".parse().unwrap()));
        assert!(is_restricted_ip(&"10.0.0.1".parse().unwrap()));
        assert!(is_restricted_ip(&"172.16.0.1".parse().unwrap()));
        assert!(is_restricted_ip(&"192.168.1.1".parse().unwrap()));
        assert!(is_restricted_ip(&"169.254.169.254".parse().unwrap()));
        assert!(is_restricted_ip(&"169.254.1.1".parse().unwrap()));
        assert!(is_restricted_ip(&"100.64.0.1".parse().unwrap()));
        assert!(!is_restricted_ip(&"100.63.0.1".parse().unwrap()));
        assert!(!is_restricted_ip(&"100.128.0.1".parse().unwrap()));
        assert!(is_restricted_ip(&"fc00::1".parse().unwrap()));
        assert!(is_restricted_ip(&"fd12:3456::1".parse().unwrap()));
        assert!(is_restricted_ip(&"::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_restricted_ip(&"::ffff:10.0.0.1".parse().unwrap()));
        assert!(is_restricted_ip(&"::ffff:169.254.169.254".parse().unwrap()));
        assert!(is_restricted_ip(&"::ffff:192.168.1.1".parse().unwrap()));
        assert!(is_restricted_ip(&"::".parse().unwrap()));
        assert!(!is_restricted_ip(&"8.8.8.8".parse().unwrap()));
        assert!(!is_restricted_ip(&"1.1.1.1".parse().unwrap()));
        assert!(!is_restricted_ip(&"93.184.216.34".parse().unwrap()));
        assert!(!is_restricted_ip(&"2606:4700::1".parse().unwrap()));
    }

    #[test]
    fn restricted_dns_result_is_denied_without_proxy_opt_in() {
        let ip = "198.18.0.1".parse().unwrap();
        let err = validate_dns_resolved_ip("github.com", &ip, None, "fetch_url")
            .expect_err("fake-IP DNS result must be denied by default");
        assert_eq!(err.kind(), WebErrorKind::PolicyDenied);
        assert!(
            err.to_string()
                .contains("resolved IP 198.18.0.1 is a restricted address")
        );
    }

    #[test]
    fn proxy_opt_in_allows_restricted_dns_for_matching_host() {
        let policy = codesmith_agent_runtime::network_policy::NetworkPolicy {
            default: Decision::Allow.into(),
            allow: Vec::new(),
            deny: Vec::new(),
            proxy: vec!["github.com".to_string()],
            audit: false,
        };
        let decider = NetworkPolicyDecider::new(policy, None);
        let ip = "198.18.0.1".parse().unwrap();
        validate_dns_resolved_ip("github.com", &ip, Some(&decider), "fetch_url")
            .expect("proxy opt-in should allow fake-IP DNS for matching host");
    }

    #[test]
    fn proxy_opt_in_does_not_allow_unlisted_host() {
        let policy = codesmith_agent_runtime::network_policy::NetworkPolicy {
            default: Decision::Allow.into(),
            allow: Vec::new(),
            deny: Vec::new(),
            proxy: vec!["github.com".to_string()],
            audit: false,
        };
        let decider = NetworkPolicyDecider::new(policy, None);
        let ip = "198.18.0.1".parse().unwrap();
        let err = validate_dns_resolved_ip("example.com", &ip, Some(&decider), "fetch_url")
            .expect_err("proxy opt-in must be scoped to configured hosts");
        assert_eq!(err.kind(), WebErrorKind::PolicyDenied);
    }

    #[tokio::test]
    async fn proxy_opt_in_does_not_allow_restricted_ip_literal() {
        let policy = codesmith_agent_runtime::network_policy::NetworkPolicy {
            default: Decision::Allow.into(),
            allow: Vec::new(),
            deny: Vec::new(),
            proxy: vec!["198.18.0.1".to_string()],
            audit: false,
        };
        let decider = NetworkPolicyDecider::new(policy, None);
        let err = HttpWebService::default()
            .fetch(
                FetchRequest::new("http://198.18.0.1/status")
                    .with_policy(Some(decider))
                    .for_tool("fetch_url"),
            )
            .await
            .expect_err("literal restricted IP URLs must stay blocked");
        assert!(
            err.to_string()
                .contains("IP 198.18.0.1 is a restricted address")
        );
    }

    #[test]
    fn proxy_dns_allow_is_audited() {
        use codesmith_agent_runtime::network_policy::NetworkAuditor;
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let auditor = NetworkAuditor::new(dir.path().join("audit.log"), true);
        let policy = codesmith_agent_runtime::network_policy::NetworkPolicy {
            default: Decision::Allow.into(),
            allow: Vec::new(),
            deny: Vec::new(),
            proxy: vec!["github.com".to_string()],
            audit: true,
        };
        let decider = NetworkPolicyDecider::new(policy, Some(auditor));
        let ip = "198.18.0.1".parse().unwrap();

        validate_dns_resolved_ip("github.com", &ip, Some(&decider), "fetch_url")
            .expect("proxy DNS allow");

        let body = std::fs::read_to_string(dir.path().join("audit.log")).expect("audit log");
        assert!(body.contains("github.com"));
        assert!(body.contains("TrustedProxyFakeIp-Allow"));
    }

    #[tokio::test]
    async fn redirect_hop_policy_is_rechecked_before_connecting() {
        // The redirect loop re-validates every hop before any connection:
        // a redirect Location to a policy-denied host must produce a
        // PolicyDenied error naming that host, without a DNS lookup or
        // connection attempt (offline-safe: no listener is involved).
        let url = reqwest::Url::parse("https://denied.example/landing").unwrap();
        let req = FetchRequest::new(url.as_str())
            .with_policy(Some(prompt_decider(&[], &["denied.example"])))
            .for_tool("web_run");
        let err = HttpWebService::default()
            .fetch(req)
            .await
            .expect_err("redirect target on the deny list must be blocked");
        assert_eq!(err.kind(), WebErrorKind::PolicyDenied);
        assert!(err.to_string().contains("denied.example"));
    }

    // === SERP parsing (ported from the pre-seam web_search tool) ==========

    #[test]
    fn bing_ckurl_with_html_entities_decodes_real_url() {
        // Regression guard: Bing /ck/a redirect hrefs are HTML-entity-encoded
        // (`&amp;`). normalize_bing_url must decode entities before extracting
        // the `u=` base64 payload, otherwise the real URL is never recovered
        // and the result's root domain collapses to bing.com (then dropped as
        // spam → 0 results for the default Bing backend).
        let href = "https://www.bing.com/ck/a?!&amp;&amp;p=abc&amp;u=a1aHR0cHM6Ly9ydXN0LWxhbmcub3JnLw&amp;ntb=1";
        assert_eq!(normalize_bing_url(href), "https://rust-lang.org/");
    }

    #[test]
    fn root_domain_strips_subdomain_keeps_two_labels() {
        assert_eq!(
            root_domain("https://astralia.forumgratuit.org/path/page").as_deref(),
            Some("forumgratuit.org"),
        );
        assert_eq!(
            root_domain("http://www.example.com/").as_deref(),
            Some("example.com"),
        );
        assert_eq!(
            root_domain("https://example.com").as_deref(),
            Some("example.com")
        );
    }

    #[test]
    fn root_domain_handles_port_and_userinfo() {
        assert_eq!(
            root_domain("http://user:pass@blog.example.com:8080/x").as_deref(),
            Some("example.com"),
        );
    }

    #[test]
    fn root_domain_returns_none_for_garbage() {
        assert!(
            root_domain("not-a-url").as_deref().is_some(),
            "bare token is treated as host"
        );
        assert!(root_domain("https:///path").is_none());
    }

    #[test]
    fn spam_detector_flags_single_domain_dominance() {
        // #964 reproduction: 5/5 results from the same low-quality host.
        let r = vec![
            entry("https://astralia.forumgratuit.org/page1"),
            entry("https://russia.forumgratuit.org/page2"),
            entry("https://other.forumgratuit.org/page3"),
            entry("https://hello.forumgratuit.org/page4"),
            entry("https://world.forumgratuit.org/page5"),
        ];
        assert!(is_likely_spam_results(&r));
    }

    #[test]
    fn spam_detector_passes_diverse_serp() {
        let r = vec![
            entry("https://example.com/a"),
            entry("https://wikipedia.org/b"),
            entry("https://stackoverflow.com/c"),
            entry("https://reddit.com/d"),
            entry("https://example.com/e"),
        ];
        assert!(!is_likely_spam_results(&r));
    }

    #[test]
    fn spam_detector_passes_short_result_set() {
        // Two results from the same domain isn't enough signal — false
        // positives on legitimate two-link answers (docs + homepage) would
        // hurt more than letting them through.
        let r = vec![
            entry("https://example.com/a"),
            entry("https://example.com/b"),
        ];
        assert!(!is_likely_spam_results(&r));
    }

    #[test]
    fn spam_detector_threshold_is_sixty_percent() {
        // 3-of-5 same domain trips the 60% threshold.
        let r3of5 = vec![
            entry("https://spam.example.com/a"),
            entry("https://spam.example.com/b"),
            entry("https://spam.example.com/c"),
            entry("https://other.com/d"),
            entry("https://third.com/e"),
        ];
        assert!(is_likely_spam_results(&r3of5));
        // 2-of-5 does NOT trip the threshold.
        let r2of5 = vec![
            entry("https://spam.example.com/a"),
            entry("https://spam.example.com/b"),
            entry("https://other.com/c"),
            entry("https://third.com/d"),
            entry("https://fourth.com/e"),
        ];
        assert!(!is_likely_spam_results(&r2of5));
    }

    #[test]
    fn decode_html_entities_handles_named_entities() {
        assert_eq!(decode_html_entities("&amp;"), "&");
        assert_eq!(decode_html_entities("&lt;"), "<");
        assert_eq!(decode_html_entities("&gt;"), ">");
        assert_eq!(decode_html_entities("&quot;"), "\"");
        assert_eq!(decode_html_entities("&apos;"), "'");
        assert_eq!(decode_html_entities("&nbsp;"), " ");
        assert_eq!(decode_html_entities("&copy;"), "\u{00A9}");
        assert_eq!(decode_html_entities("&mdash;"), "\u{2014}");
    }

    #[test]
    fn decode_html_entities_handles_decimal_numeric_references() {
        assert_eq!(decode_html_entities("&#65;"), "A");
        assert_eq!(decode_html_entities("&#60;"), "<");
        assert_eq!(decode_html_entities("&#8211;"), "\u{2013}");
    }

    #[test]
    fn decode_html_entities_handles_hex_numeric_references() {
        assert_eq!(decode_html_entities("&#x41;"), "A");
        assert_eq!(decode_html_entities("&#x3C;"), "<");
        assert_eq!(decode_html_entities("&#x2014;"), "\u{2014}");
    }

    #[test]
    fn decode_html_entities_passthrough_unknown() {
        assert_eq!(decode_html_entities("&unknown;"), "&unknown;");
    }

    #[test]
    fn decode_html_entities_mixed_content() {
        let input = "Hello &amp; welcome to &quot;Rust&apos;s world&quot; &mdash; enjoy!";
        let expected = "Hello & welcome to \"Rust's world\" \u{2014} enjoy!";
        assert_eq!(decode_html_entities(input), expected);
    }

    #[test]
    fn truncate_error_body_truncates_long_body() {
        let body = "a".repeat(ERROR_BODY_PREVIEW_BYTES + 100);
        let truncated = truncate_error_body(&body);
        assert!(truncated.len() <= ERROR_BODY_PREVIEW_BYTES + 3);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn truncate_error_body_keeps_short_body_intact() {
        let body = "short error";
        assert_eq!(truncate_error_body(body), body);
    }

    #[test]
    fn sanitize_error_body_strips_html_and_control_chars() {
        let body = "<p>error</p>\x00\x01\x02";
        let sanitized = sanitize_error_body(body);
        assert_eq!(sanitized, "error");
    }

    #[test]
    fn sanitize_error_body_redacts_bearer_tokens() {
        let body = r#"{"error":"bad token","authorization":"Bearer test-token/with+chars="}"#;
        let sanitized = sanitize_error_body(body);
        assert!(!sanitized.contains("test-token/with+chars="));
        assert!(sanitized.contains("Bearer [REDACTED]"));
    }

    // === API provider payload/parse helpers ===============================

    #[test]
    fn parse_generic_results_extracts_baidu_references() {
        let body = json!({
            "references": [
                {
                    "title": "Rust 官方文档",
                    "url": "https://www.rust-lang.org/",
                    "content": "Rust 是一门注重性能和可靠性的语言。"
                },
                {
                    "title": "Cargo Book",
                    "url": "https://doc.rust-lang.org/cargo/",
                    "snippet": "Cargo is Rust's package manager."
                },
                {
                    "title": "No URL",
                    "content": "missing url"
                }
            ]
        });

        let results = parse_generic_results(
            &body["references"],
            &[
                &["title", "name"],
                &["url", "link"],
                &["content", "snippet", "summary"],
            ],
            10,
        );

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust 官方文档");
        assert_eq!(results[0].url, "https://www.rust-lang.org/");
        assert_eq!(
            results[0].snippet.as_deref(),
            Some("Rust 是一门注重性能和可靠性的语言。")
        );
        assert_eq!(results[1].url, "https://doc.rust-lang.org/cargo/");
    }

    #[test]
    fn baidu_search_payload_uses_official_search_source() {
        let payload = baidu_search_payload("Rust cargo workspace", 3);
        assert_eq!(
            payload.get("search_source").and_then(|v| v.as_str()),
            Some("baidu_search_v2")
        );
        assert_eq!(
            payload
                .get("messages")
                .and_then(|v| v.as_array())
                .and_then(|messages| messages.first())
                .and_then(|message| message.get("content"))
                .and_then(|v| v.as_str()),
            Some("Rust cargo workspace")
        );
        assert_eq!(
            payload
                .get("resource_type_filter")
                .and_then(|v| v.as_array())
                .and_then(|filters| filters.first())
                .and_then(|filter| filter.get("top_k"))
                .and_then(|v| v.as_u64()),
            Some(3)
        );
    }

    #[test]
    fn volcengine_extract_text_skips_non_text_content_blocks() {
        let body = json!({
            "output": [
                {
                    "type": "message",
                    "content": [
                        {"type": "reasoning", "summary": "thinking first"},
                        {"type": "output_text", "text": "{\"results\":[]}"}
                    ]
                }
            ]
        });
        assert_eq!(
            volcengine_extract_text(&body).as_deref(),
            Some("{\"results\":[]}")
        );
    }

    // === provider_native =================================================

    #[tokio::test]
    async fn provider_native_without_native_config_fails_loud() {
        // The backend speaks the provider's Messages API; without the host's
        // connection details it must not quietly fall back to a scrape.
        let svc = HttpWebService::new(SearchProvider::ProviderNative, None);
        let err = svc
            .search(SearchRequest::new("anything"))
            .await
            .expect_err("missing native config must surface");
        assert_eq!(err.kind(), WebErrorKind::Config);
        assert!(
            err.to_string().contains("provider_native"),
            "error must name the backend; got `{err}`"
        );
    }

    #[test]
    fn provider_native_hosts_follow_the_configured_base_url() {
        let svc = HttpWebService::new(SearchProvider::ProviderNative, None).with_native_search(
            NativeSearchConfig {
                base_url: "https://open.bigmodel.cn/api/anthropic".into(),
                api_key: "k".into(),
                model: "glm-5.3".into(),
            },
        );
        assert_eq!(svc.search_hosts(), vec!["open.bigmodel.cn".to_string()]);
    }

    #[test]
    fn messages_endpoint_does_not_double_v1() {
        assert_eq!(
            messages_endpoint("https://open.bigmodel.cn/api/anthropic"),
            "https://open.bigmodel.cn/api/anthropic/v1/messages"
        );
        assert_eq!(
            messages_endpoint("https://api.anthropic.com/v1"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            messages_endpoint("https://host/v1/messages/"),
            "https://host/v1/messages"
        );
    }

    #[test]
    fn provider_native_parses_bigmodel_stringified_results() {
        // Real BigModel shape: content[] carries a `tool_result` whose
        // `content` is a repr of a list of dicts. Python switches to double
        // quotes when a title contains an apostrophe, so both styles appear.
        let body = json!({
            "content": [
                {"type": "thinking", "thinking": "searching"},
                {"type": "tool_result", "tool_use_id": "call_1", "content":
                    "[{'text': [{'title': 'Tokio - An asynchronous Rust runtime', 'link': 'https://tokio.rs', 'content': 'Tokio is a runtime for writing reliable applications.', 'refer': 'ref_1'}, {\"title\": \"Tokio's tutorial\", 'link': 'https://tokio.rs/tokio/tutorial', 'content': 'An in-depth tutorial.', 'refer': 'ref_2'}]}]"},
                {"type": "text", "text": "Tokio is a runtime (source: https://tokio.rs)."}
            ]
        });
        let hits = parse_provider_native_results(&body, 5);
        assert_eq!(hits.len(), 2, "one hit per distinct link; got {hits:?}");
        assert_eq!(hits[0].title, "Tokio - An asynchronous Rust runtime");
        assert_eq!(hits[0].url, "https://tokio.rs");
        assert_eq!(
            hits[0].snippet.as_deref(),
            Some("Tokio is a runtime for writing reliable applications.")
        );
        assert_eq!(hits[1].title, "Tokio's tutorial");
        assert_eq!(hits[1].url, "https://tokio.rs/tokio/tutorial");
    }

    #[test]
    fn provider_native_parses_anthropic_result_objects_and_dedupes() {
        let body = json!({
            "content": [
                {"type": "web_search_tool_result", "content": [
                    {"type": "web_search_result", "title": "Tokio", "url": "https://tokio.rs"},
                    {"type": "web_search_result", "title": "Docs", "url": "https://docs.rs/tokio"}
                ]},
                {"type": "text", "text": "summary", "citations": [
                    {"type": "web_search_result_location", "title": "Tokio", "url": "https://tokio.rs"}
                ]}
            ]
        });
        let hits = parse_provider_native_results(&body, 10);
        assert_eq!(
            hits.len(),
            2,
            "citation duplicate of a result URL is dropped"
        );
        assert_eq!(hits[0].url, "https://tokio.rs");
        assert_eq!(hits[1].url, "https://docs.rs/tokio");
    }

    #[test]
    fn provider_native_caps_results_to_request_max() {
        let body = json!({"content": [{"type": "web_search_tool_result", "content": [
            {"title": "a", "url": "https://a.example"},
            {"title": "b", "url": "https://b.example"},
            {"title": "c", "url": "https://c.example"}
        ]}]});
        assert_eq!(parse_provider_native_results(&body, 2).len(), 2);
    }

    #[test]
    fn provider_native_error_message_reads_anthropic_and_business_errors() {
        assert_eq!(
            provider_native_error_message(&json!({
                "error": {"type": "invalid_request_error", "message": "unknown tool"}
            })),
            Some("Provider-native web search error (invalid_request_error): unknown tool".into())
        );
        assert_eq!(
            provider_native_error_message(&json!({
                "error": {"code": "1113", "message": "insufficient balance"}
            })),
            Some("Provider-native web search error (1113): insufficient balance".into())
        );
        assert_eq!(provider_native_error_message(&json!({"content": []})), None);
    }
}
