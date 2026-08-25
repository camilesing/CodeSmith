//! Async MCP (Model Context Protocol) Implementation
//!
//! This module provides full async support for MCP servers with:
//! - Connection pooling for server reuse
//! - Automatic tool discovery via `tools/list`
//! - Configurable timeouts per-server and globally

// `libc::kill` (best-effort SIGTERM of spawned MCP servers) is an inherent
// `unsafe` call. The runtime crate is otherwise `unsafe_code`-denied, so this
// module opts back in locally.
#![allow(unsafe_code)]

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex as TokioMutex;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{HeaderName, HeaderValue};

use crate::child_env;
use crate::mcp_oauth::{McpOAuthConfig, OAuthState};
use crate::network_policy::{Decision, NetworkPolicyDecider, host_from_url};
use crate::utils::write_atomic;

pub use crate::mcp_oauth::{McpOAuthToken, default_secrets as oauth_default_secrets};

// === Error diagnostics helpers (#71) ===

/// Bytes of a non-2xx response body to surface in connection errors.
const ERROR_BODY_PREVIEW_BYTES: usize = 200;
const MCP_HTTP_ACCEPT: &str = "application/json, text/event-stream";
const MAX_MCP_DESCRIPTION_LENGTH: usize = 2048;
const MCP_AUTH_CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const MCP_LOCAL_CONNECTION_BATCH_SIZE: usize = 3;
const MCP_REMOTE_CONNECTION_BATCH_SIZE: usize = 20;
const ALLOWED_IDE_TOOLS: &[&str] = &["mcp__ide__getDiagnostics", "mcp__ide__getOpenEditorFiles"];

pub fn truncate_mcp_description(description: &str) -> String {
    if description.chars().count() <= MAX_MCP_DESCRIPTION_LENGTH {
        return description.to_string();
    }
    description
        .chars()
        .take(MAX_MCP_DESCRIPTION_LENGTH)
        .collect()
}

fn qualify_mcp_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

fn parse_mcp_tool_name(prefixed_name: &str) -> Result<(&str, &str)> {
    if let Some(rest) = prefixed_name.strip_prefix("mcp__") {
        let Some((server, tool)) = rest.split_once("__") else {
            anyhow::bail!("Invalid MCP tool name format: {prefixed_name}");
        };
        if server.is_empty() || tool.is_empty() {
            anyhow::bail!("Invalid MCP tool name format: {prefixed_name}");
        }
        return Ok((server, tool));
    }

    if let Some(rest) = prefixed_name.strip_prefix("mcp_") {
        let Some((server, tool)) = rest.split_once('_') else {
            anyhow::bail!("Invalid MCP tool name format: {prefixed_name}");
        };
        if server.is_empty() || tool.is_empty() {
            anyhow::bail!("Invalid MCP tool name format: {prefixed_name}");
        }
        return Ok((server, tool));
    }

    anyhow::bail!("Invalid MCP tool name: {prefixed_name}");
}

pub fn parse_mcp_tool_model_name(prefixed_name: &str) -> Result<(&str, &str)> {
    parse_mcp_tool_name(prefixed_name)
}

fn is_allowed_ide_tool(model_name: &str) -> bool {
    ALLOWED_IDE_TOOLS.contains(&model_name)
}

fn with_default_mcp_http_headers(
    request: reqwest::RequestBuilder,
    json_body: bool,
) -> reqwest::RequestBuilder {
    let request = request.header(ACCEPT, MCP_HTTP_ACCEPT);
    if json_body {
        request.header(CONTENT_TYPE, "application/json")
    } else {
        request
    }
}

fn validate_mcp_config_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        anyhow::bail!("MCP config path cannot be empty");
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        anyhow::bail!("MCP config path cannot contain '..' components");
    }
    Ok(())
}

/// Predicate for custom-header passes.
///
/// We accept whatever each HTTP implementation would accept, but with extra
/// rules:
///
/// 1. Reject empty / whitespace-only keys.
/// 2. Reject keys that duplicate protocol framing headers. The default HTTP
///    transport reserves `Accept` and `Content-Type`; WebSocket handshakes also
///    reserve upgrade-specific headers.
/// 3. Reject values containing ASCII CR or LF.
///
/// Returning `false` means "skip this header"; the rest of the request still
/// goes out.
fn is_safe_custom_header_with_reserved(key: &str, value: &str, reserved: &[&str]) -> bool {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return false;
    }
    if reserved
        .iter()
        .any(|header| trimmed.eq_ignore_ascii_case(header))
    {
        return false;
    }
    !value.contains('\r') && !value.contains('\n')
}

fn is_safe_custom_header(key: &str, value: &str) -> bool {
    is_safe_custom_header_with_reserved(key, value, &["accept", "content-type"])
}

fn is_safe_websocket_header(key: &str, value: &str) -> bool {
    is_safe_custom_header_with_reserved(
        key,
        value,
        &[
            "accept",
            "connection",
            "content-type",
            "host",
            "sec-websocket-accept",
            "sec-websocket-extensions",
            "sec-websocket-key",
            "sec-websocket-protocol",
            "sec-websocket-version",
            "upgrade",
        ],
    )
}

fn apply_safe_custom_headers(
    mut request: reqwest::RequestBuilder,
    headers: &HashMap<String, String>,
) -> reqwest::RequestBuilder {
    for (key, value) in headers {
        if !is_safe_custom_header(key, value) {
            tracing::warn!(
                target: "mcp",
                "skipping unsafe MCP header {:?} (empty/control-char/reserved)",
                key
            );
            continue;
        }
        request = request.header(key.as_str(), value.as_str());
    }
    request
}

fn apply_safe_websocket_headers(
    mut request: tokio_tungstenite::tungstenite::http::Request<()>,
    headers: &HashMap<String, String>,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    for (key, value) in headers {
        if !is_safe_websocket_header(key, value) {
            tracing::warn!(
                target: "mcp",
                "skipping unsafe MCP WebSocket header {:?} (empty/control-char/reserved)",
                key
            );
            continue;
        }
        let Ok(name) = HeaderName::from_bytes(key.as_bytes()) else {
            tracing::warn!(target: "mcp", "skipping invalid MCP WebSocket header {:?}", key);
            continue;
        };
        let Ok(value) = HeaderValue::from_str(value) else {
            tracing::warn!(target: "mcp", "skipping invalid MCP WebSocket header value for {:?}", key);
            continue;
        };
        request.headers_mut().insert(name, value);
    }
    request
}

fn with_websocket_bearer_header(
    mut request: tokio_tungstenite::tungstenite::http::Request<()>,
    url: &str,
    headers: &HashMap<String, String>,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    if claude_ai_host(url)
        && !headers
            .keys()
            .any(|key| key.eq_ignore_ascii_case("authorization"))
        && let Some(token) = configured_claude_access_token(headers)
        && let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}"))
    {
        request
            .headers_mut()
            .insert(HeaderName::from_static("authorization"), value);
    }
    request
}

fn claude_ai_host(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host == "claude.ai" || host.ends_with(".claude.ai"))
}

fn configured_claude_access_token(headers: &HashMap<String, String>) -> Option<String> {
    if let Ok(token) = std::env::var("CODESMITH_MCP_CLAUDE_ACCESS_TOKEN")
        && !token.trim().is_empty()
    {
        return Some(token);
    }
    headers.iter().find_map(|(key, value)| {
        if key.eq_ignore_ascii_case("authorization") {
            value
                .trim()
                .strip_prefix("Bearer ")
                .map(|token| token.trim().to_string())
        } else {
            None
        }
    })
}

async fn refresh_claude_access_token() -> Result<Option<String>> {
    let Ok(command) = std::env::var("CODESMITH_MCP_CLAUDE_REFRESH_COMMAND") else {
        return Ok(None);
    };
    if command.trim().is_empty() {
        return Ok(None);
    }
    let parts = shlex::split(&command).context("Invalid CODESMITH_MCP_CLAUDE_REFRESH_COMMAND")?;
    let Some((program, args)) = parts.split_first() else {
        return Ok(None);
    };
    let output = tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .with_context(|| "Failed to run CODESMITH_MCP_CLAUDE_REFRESH_COMMAND")?;
    if !output.status.success() {
        anyhow::bail!(
            "CODESMITH_MCP_CLAUDE_REFRESH_COMMAND failed with status {}",
            output.status
        );
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        Ok(None)
    } else {
        Ok(Some(token))
    }
}

fn with_bearer_header(
    request: reqwest::RequestBuilder,
    url: &str,
    headers: &HashMap<String, String>,
) -> reqwest::RequestBuilder {
    if claude_ai_host(url)
        && !headers
            .keys()
            .any(|key| key.eq_ignore_ascii_case("authorization"))
        && let Some(token) = configured_claude_access_token(headers)
    {
        return request.bearer_auth(token);
    }
    request
}

async fn maybe_refresh_claude_headers(
    url: &str,
    status: StatusCode,
    headers: &mut HashMap<String, String>,
) -> Result<bool> {
    if status != StatusCode::UNAUTHORIZED || !claude_ai_host(url) {
        return Ok(false);
    }
    let Some(token) = refresh_claude_access_token().await? else {
        return Ok(false);
    };
    headers.insert("Authorization".to_string(), format!("Bearer {token}"));
    Ok(true)
}

/// One OAuth refresh-token round trip after a 401 (#2190). Only touches a
/// header we injected at connect time — `try_refresh_matching` compares
/// against the stored access token, so a user-configured Authorization
/// header is left alone. Returns whether the header was replaced (the
/// caller retries the request once).
async fn maybe_refresh_oauth_headers(
    client: &reqwest::Client,
    headers: &mut HashMap<String, String>,
    oauth: &OAuthState,
) -> bool {
    let Some(current) = headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
        .map(|(_, value)| value.clone())
    else {
        return false;
    };
    match crate::mcp_oauth::try_refresh_matching(
        client,
        &crate::mcp_oauth::default_secrets(),
        oauth,
        &current,
    )
    .await
    {
        Ok(Some(token)) => {
            tracing::info!(
                target: "mcp",
                server = %oauth.server,
                "oauth token refreshed after 401; retrying request"
            );
            headers.insert(
                "Authorization".to_string(),
                format!("Bearer {}", token.access_token),
            );
            true
        }
        Ok(None) => false,
        Err(err) => {
            tracing::warn!(
                target: "mcp",
                server = %oauth.server,
                error = %err,
                "oauth token refresh failed; re-run `codesmith mcp auth {}` if the server keeps rejecting requests",
                oauth.server
            );
            false
        }
    }
}

/// Mask a URL so any embedded credentials in the userinfo portion (e.g.
/// `https://user:secret@host`) are replaced with `***`. Failures fall back to
/// the original string so we don't lose context — we never want masking to
/// produce an empty error.
fn mask_url_secrets(url: &str) -> String {
    if let Ok(parsed) = reqwest::Url::parse(url) {
        let mut clone = parsed.clone();
        if !parsed.username().is_empty() || parsed.password().is_some() {
            let _ = clone.set_username("***");
            let _ = clone.set_password(Some("***"));
        }
        return clone.to_string();
    }
    url.to_string()
}

/// Redact the userinfo segment (`username[:password]@…` portion) from
/// a proxy URL so it can be safely included in `tracing::warn!` output
/// without leaking the
/// password into the on-disk log. URLs without userinfo are returned
/// unchanged. Garbage input (no `://` scheme separator) is also returned
/// unchanged — the malformed-URL warning path is the only caller, so an
/// unparseable input is already the failure case.
fn redact_proxy_userinfo(proxy_url: &str) -> String {
    let Some(scheme_end) = proxy_url.find("://") else {
        return proxy_url.to_string();
    };
    let after_scheme = scheme_end + 3;
    // The userinfo segment ends at the next `@`, but only if that `@`
    // comes before the next `/`, `?`, or `#` (otherwise the `@` is in a
    // path / query and the URL has no userinfo at all).
    let rest = &proxy_url[after_scheme..];
    let at_idx = rest.find('@');
    let path_idx = rest.find(['/', '?', '#']);
    let userinfo_end = match (at_idx, path_idx) {
        (Some(a), Some(p)) if a < p => Some(a),
        (Some(a), None) => Some(a),
        _ => None,
    };
    if let Some(end) = userinfo_end {
        let mut out = String::with_capacity(proxy_url.len());
        out.push_str(&proxy_url[..after_scheme]);
        out.push_str("***@");
        out.push_str(&rest[end + 1..]);
        out
    } else {
        proxy_url.to_string()
    }
}

/// Mask any obvious token-like substrings in a body excerpt before surfacing
/// it. Conservative: replaces `Bearer <token>` and `api_key=...` shapes.
fn redact_body_preview(body: &str) -> String {
    let mut out = body.to_string();
    if let Some(idx) = out.to_lowercase().find("bearer ") {
        let tail_start = idx + "bearer ".len();
        if tail_start < out.len() {
            let end = out[tail_start..]
                .find(|c: char| c.is_whitespace() || c == '"' || c == ',')
                .map_or(out.len(), |off| tail_start + off);
            out.replace_range(tail_start..end, "***");
        }
    }
    for needle in ["api_key=", "apikey=", "api-key=", "token="] {
        if let Some(idx) = out.to_lowercase().find(needle) {
            let tail_start = idx + needle.len();
            let end = out[tail_start..]
                .find(|c: char| c.is_whitespace() || c == '&' || c == '"' || c == ',')
                .map_or(out.len(), |off| tail_start + off);
            out.replace_range(tail_start..end, "***");
        }
    }
    out
}

/// Read up to `max_bytes` of a reqwest Response body and produce a single-line
/// excerpt suitable for an error message. Best-effort — if the body can't be
/// read, returns the literal string `<no body>`.
async fn bounded_body_excerpt(response: reqwest::Response, max_bytes: usize) -> String {
    let body_text = response.text().await.unwrap_or_default();
    if body_text.is_empty() {
        return "<no body>".to_string();
    }
    let trimmed: String = body_text.chars().take(max_bytes).collect();
    let suffix = if body_text.len() > trimmed.len() {
        "…"
    } else {
        ""
    };
    let one_line = trimmed.replace(['\n', '\r'], " ");
    format!("{}{}", redact_body_preview(&one_line), suffix)
}

// === Configuration Types ===

/// Full MCP configuration from mcp.json
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct McpConfig {
    #[serde(default)]
    pub timeouts: McpTimeouts,
    #[serde(default, alias = "mcpServers")]
    pub servers: HashMap<String, McpServerConfig>,
}

/// Global timeout configuration
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[allow(clippy::struct_field_names)]
pub struct McpTimeouts {
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout: u64,
    #[serde(default = "default_execute_timeout")]
    pub execute_timeout: u64,
    #[serde(default = "default_read_timeout")]
    pub read_timeout: u64,
}

fn default_connect_timeout() -> u64 {
    10
}
fn default_execute_timeout() -> u64 {
    60
}
fn default_read_timeout() -> u64 {
    120
}

impl Default for McpTimeouts {
    fn default() -> Self {
        Self {
            connect_timeout: default_connect_timeout(),
            execute_timeout: default_execute_timeout(),
            read_timeout: default_read_timeout(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpTransportKind {
    StreamableHttp,
    Sse,
    SseIde,
    WebSocket,
    WebSocketIde,
}

impl McpTransportKind {
    fn parse(transport: Option<&str>) -> Result<Self> {
        let Some(raw) = transport else {
            return Ok(Self::StreamableHttp);
        };
        let normalized = raw.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "" | "http" | "streamable" | "streamable-http" => Ok(Self::StreamableHttp),
            "sse" => Ok(Self::Sse),
            "sse-ide" => Ok(Self::SseIde),
            "ws" | "websocket" => Ok(Self::WebSocket),
            "ws-ide" | "websocket-ide" => Ok(Self::WebSocketIde),
            _ => anyhow::bail!(
                "Unsupported MCP transport '{raw}'. Supported values: http, streamable-http, sse, sse-ide, ws, ws-ide"
            ),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::StreamableHttp => "http/sse",
            Self::Sse => "sse",
            Self::SseIde => "sse-ide",
            Self::WebSocket => "ws",
            Self::WebSocketIde => "ws-ide",
        }
    }

    fn is_sse(self) -> bool {
        matches!(self, Self::Sse | Self::SseIde)
    }

    fn is_websocket(self) -> bool {
        matches!(self, Self::WebSocket | Self::WebSocketIde)
    }

    fn is_ide(self) -> bool {
        matches!(self, Self::SseIde | Self::WebSocketIde)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpServerConfig {
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub url: Option<String>,
    /// Optional explicit URL transport override.
    ///
    /// By default URL-based MCP servers use Streamable HTTP first and fall
    /// back to legacy SSE only when the server rejects Streamable HTTP with
    /// a known incompatible status. Set this to `"sse"`/`"sse-ide"` for
    /// legacy SSE endpoints that must start with endpoint discovery, or
    /// `"ws"`/`"ws-ide"` for WebSocket MCP endpoints.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(default)]
    pub connect_timeout: Option<u64>,
    #[serde(default)]
    pub execute_timeout: Option<u64>,
    #[serde(default)]
    pub read_timeout: Option<u64>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub enabled_tools: Vec<String>,
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    /// Extra HTTP headers sent with every request to this MCP server.
    /// Only the HTTP transports (streamable HTTP today; SSE in a
    /// follow-up) honor this — `command`-based stdio servers ignore it.
    ///
    /// Mirrors the `headers` field that Claude Code, Codex, and
    /// OpenCode already accept in their MCP config formats. Use it to
    /// authenticate against gateways that require a Bearer token or
    /// API key, e.g.:
    ///
    /// ```jsonc
    /// "huggingface": {
    ///     "url": "https://huggingface.co/api/mcp",
    ///     "headers": { "Authorization": "Bearer ${HF_TOKEN}" }
    /// }
    /// ```
    ///
    /// Header keys and values are passed through as-is — we do not
    /// substitute environment variables in v0.8.31. If you store a
    /// real token here, the value lives in plain text in
    /// `~/.codesmith/mcp.json`; treat that file with the same care
    /// as any other secret-bearing config.
    #[serde(default)]
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    /// OAuth device-flow configuration for URL transports (#2190). When
    /// set, the stored token (see `codesmith mcp auth`) is injected as the
    /// `Authorization` header at connect time unless `headers` already
    /// carries one, and the HTTP transports refresh it once on 401.
    /// Stdio servers ignore this field — pass credentials via `env`.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpOAuthConfig>,
}

fn default_enabled() -> bool {
    true
}

impl McpServerConfig {
    pub fn effective_connect_timeout(&self, global: &McpTimeouts) -> u64 {
        self.connect_timeout.unwrap_or(global.connect_timeout)
    }

    pub fn effective_execute_timeout(&self, global: &McpTimeouts) -> u64 {
        self.execute_timeout.unwrap_or(global.execute_timeout)
    }

    pub fn effective_read_timeout(&self, global: &McpTimeouts) -> u64 {
        self.read_timeout.unwrap_or(global.read_timeout)
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled && !self.disabled
    }

    pub fn is_tool_enabled(&self, tool_name: &str) -> bool {
        let allowed = if self.enabled_tools.is_empty() {
            true
        } else {
            self.enabled_tools.iter().any(|t| t == tool_name)
        };
        if !allowed {
            return false;
        }
        !self.disabled_tools.iter().any(|t| t == tool_name)
    }
}

// === MCP Tool Definition ===

/// Tool discovered from an MCP server
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema", default)]
    pub input_schema: serde_json::Value,
}

/// Resource discovered from an MCP server
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpResource {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "mimeType", default)]
    pub mime_type: Option<String>,
}

/// Resource template discovered from an MCP server
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpResourceTemplate {
    #[serde(rename = "uriTemplate")]
    pub uri_template: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "mimeType", default)]
    pub mime_type: Option<String>,
}

/// Prompt discovered from an MCP server
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpPrompt {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub arguments: Vec<McpPromptArgument>,
}

/// Argument for an MCP prompt
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpPromptArgument {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
}

// === Connection State ===

/// State of an MCP connection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Ready,
    Disconnected,
}

// === McpConnection - Async Connection Management ===

// === Transport Trait ===

#[async_trait::async_trait]
pub trait McpTransport: Send + Sync {
    async fn send(&mut self, msg: Vec<u8>) -> Result<()>;
    async fn recv(&mut self) -> Result<Vec<u8>>;

    /// Graceful shutdown — stdio transports send SIGTERM to the child and
    /// give it a brief window to exit before tokio's `kill_on_drop` fires
    /// SIGKILL as the backstop. Default is a no-op for non-stdio transports
    /// that have no child process. Whalescale#420.
    async fn shutdown(&mut self) {}
}

pub struct StdioTransport {
    child: Child,
    stdin: ChildStdin,
    reader: tokio::io::BufReader<ChildStdout>,
    /// Tail of stderr lines from the spawned MCP server. A background task
    /// drains the child's stderr into this buffer so a mid-run crash leaves
    /// some context behind instead of `Stdio::null` swallowing it.
    stderr_tail: Arc<StderrTail>,
}

/// How long `StdioTransport::shutdown` waits for the child to exit on SIGTERM
/// before `kill_on_drop` fires SIGKILL. Tuned short so a hung MCP server
/// can't stall TUI exit; well-behaved servers almost always exit within
/// a few hundred ms.
const STDIO_SHUTDOWN_GRACE: Duration = Duration::from_millis(2_000);

/// How many lines of MCP-server stderr to keep around for crash diagnostics.
/// Bounded so a chatty server can't grow this without limit; large enough to
/// catch typical Node/Python startup or panic output.
const STDERR_TAIL_CAPACITY: usize = 64;

/// Bounded ring buffer for the most recent stderr lines from a spawned MCP
/// server. Used by `StdioTransport` to surface server-side context when the
/// transport read side fails (server crashed, exited early, etc).
#[derive(Default)]
pub struct StderrTail {
    lines: TokioMutex<VecDeque<String>>,
}

impl StderrTail {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            lines: TokioMutex::new(VecDeque::with_capacity(STDERR_TAIL_CAPACITY)),
        })
    }

    async fn push(&self, line: String) {
        let mut buf = self.lines.lock().await;
        if buf.len() >= STDERR_TAIL_CAPACITY {
            buf.pop_front();
        }
        buf.push_back(line);
    }

    async fn snapshot(&self) -> Vec<String> {
        self.lines.lock().await.iter().cloned().collect()
    }
}

/// Format the captured stderr tail for inclusion in an error message. Empty
/// tails return `None` so the caller can fall back to its original message.
async fn format_stderr_context(tail: &StderrTail) -> Option<String> {
    let lines = tail.snapshot().await;
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "MCP server stderr (last {} line{}):\n{}",
        lines.len(),
        if lines.len() == 1 { "" } else { "s" },
        lines.join("\n"),
    ))
}

/// Best-effort SIGTERM. On Unix uses `libc::kill`; on Windows there's no
/// equivalent so we let `kill_on_drop` (TerminateProcess) handle it via the
/// subsequent Drop. Returns whether a signal was actually sent.
fn send_sigterm(child: &Child) -> bool {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            // SAFETY: pid was just obtained from `child.id()`. `libc::kill`
            // with `SIGTERM` is async-signal-safe and never observes invalid
            // memory. Worst case (pid wrap / process already gone) returns
            // ESRCH, which we deliberately ignore.
            unsafe {
                let _ = libc::kill(pid as i32, libc::SIGTERM);
            }
            return true;
        }
        false
    }
    #[cfg(not(unix))]
    {
        let _ = child;
        false
    }
}

#[async_trait::async_trait]
impl McpTransport for StdioTransport {
    async fn send(&mut self, mut msg: Vec<u8>) -> Result<()> {
        msg.push(b'\n');
        self.stdin.write_all(&msg).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = match self.reader.read_line(&mut line).await {
                Ok(b) => b,
                Err(err) => {
                    if let Some(stderr) = format_stderr_context(&self.stderr_tail).await {
                        anyhow::bail!("Stdio transport read error: {err}\n{stderr}");
                    }
                    return Err(err.into());
                }
            };
            if bytes == 0 {
                if let Some(stderr) = format_stderr_context(&self.stderr_tail).await {
                    anyhow::bail!("Stdio transport closed\n{stderr}");
                }
                anyhow::bail!("Stdio transport closed");
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            return Ok(trimmed.as_bytes().to_vec());
        }
    }

    /// Send SIGTERM and wait up to `STDIO_SHUTDOWN_GRACE` for graceful exit
    /// before letting Drop / `kill_on_drop` fire SIGKILL as the backstop.
    async fn shutdown(&mut self) {
        send_sigterm(&self.child);
        // Give the child a window to exit cleanly. Discard the result —
        // either it exits (success) or the timeout fires (Drop will SIGKILL).
        let _ = tokio::time::timeout(STDIO_SHUTDOWN_GRACE, self.child.wait()).await;
    }
}

/// Drop fallback (#420): if `shutdown` was never called explicitly, still
/// fire SIGTERM before tokio's `kill_on_drop` sends SIGKILL. The two
/// signals arrive back-to-back so well-behaved servers at least see the
/// SIGTERM first; misbehaving ones get SIGKILL'd anyway.
impl Drop for StdioTransport {
    fn drop(&mut self) {
        send_sigterm(&self.child);
    }
}

pub struct SseTransport {
    client: reqwest::Client,
    base_url: String,
    headers: HashMap<String, String>,
    /// OAuth context for the 401 refresh path; `None` for servers without
    /// an `oauth` config entry.
    oauth: Option<OAuthState>,
    endpoint_url: Option<String>,
    receiver: tokio::sync::mpsc::UnboundedReceiver<SseInbound>,
    pending_messages: VecDeque<Vec<u8>>,
}

enum SseInbound {
    Endpoint(String),
    Message(Vec<u8>),
}

struct HttpTransport {
    mode: HttpTransportMode,
    client: reqwest::Client,
    base_url: String,
    headers: HashMap<String, String>,
    oauth: Option<OAuthState>,
    cancel_token: tokio_util::sync::CancellationToken,
    endpoint_timeout: Duration,
}

enum HttpTransportMode {
    Streamable(StreamableHttpTransport),
    Sse(SseTransport),
}

struct StreamableHttpTransport {
    client: reqwest::Client,
    url: String,
    /// Extra headers applied to every outbound POST. Populated from
    /// [`McpServerConfig::headers`]; an empty map is the no-auth
    /// default. See `apply_custom_headers` for the filtering pass that
    /// runs before each request.
    headers: HashMap<String, String>,
    /// OAuth context for the 401 refresh path; `None` for servers without
    /// an `oauth` config entry.
    oauth: Option<OAuthState>,
    pending_messages: VecDeque<Vec<u8>>,
    /// Per-spec MCP session identifier returned by the server in the
    /// first response (typically the `initialize` response). Attached
    /// as the `Mcp-Session-Id` header on every subsequent outbound
    /// request so the server can correlate messages within the same
    /// session.
    session_id: Option<String>,
}

#[derive(Debug)]
enum StreamableSendError {
    Incompatible(String),
    StaleSession(String),
    Other(anyhow::Error),
}

struct WebSocketTransport {
    stream: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
}

impl WebSocketTransport {
    async fn connect(url: String, headers: HashMap<String, String>) -> Result<Self> {
        let request = url
            .as_str()
            .into_client_request()
            .with_context(|| format!("Invalid MCP WebSocket URL: {}", mask_url_secrets(&url)))?;
        let request = apply_safe_websocket_headers(request, &headers);
        let request = with_websocket_bearer_header(request, &url, &headers);
        let (stream, _) = connect_async(request).await.with_context(|| {
            format!(
                "MCP WebSocket connect failed (transport=ws url={})",
                mask_url_secrets(&url)
            )
        })?;
        Ok(Self { stream })
    }
}

#[async_trait::async_trait]
impl McpTransport for WebSocketTransport {
    async fn send(&mut self, msg: Vec<u8>) -> Result<()> {
        let text = String::from_utf8(msg).context("MCP WebSocket payload was not valid UTF-8")?;
        self.stream.send(Message::Text(text.into())).await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        loop {
            let message = self
                .stream
                .next()
                .await
                .context("WebSocket transport closed")??;
            match message {
                Message::Text(text) => return Ok(text.as_str().as_bytes().to_vec()),
                Message::Binary(bytes) => return Ok(bytes.to_vec()),
                Message::Ping(payload) => {
                    self.stream.send(Message::Pong(payload)).await?;
                }
                Message::Pong(_) | Message::Frame(_) => {}
                Message::Close(frame) => {
                    let detail = frame
                        .map(|f| format!("code={} reason={}", f.code, f.reason))
                        .unwrap_or_else(|| "no close frame".to_string());
                    anyhow::bail!("WebSocket transport closed ({detail})");
                }
            }
        }
    }

    async fn shutdown(&mut self) {
        let _ = self.stream.close(None).await;
    }
}

impl SseTransport {
    pub async fn connect(
        client: reqwest::Client,
        url: String,
        headers: HashMap<String, String>,
        cancel_token: tokio_util::sync::CancellationToken,
        endpoint_timeout: Duration,
        oauth: Option<OAuthState>,
    ) -> Result<Self> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let client_clone = client.clone();
        let url_clone = url.clone();
        let headers_clone = headers.clone();
        let wait_cancel_token = cancel_token.clone();

        tokio::spawn(async move {
            if cancel_token.is_cancelled() {
                return;
            }
            use futures_util::FutureExt;
            let result = std::panic::AssertUnwindSafe(Self::run_sse_loop(
                client_clone,
                url_clone,
                headers_clone,
                tx,
                cancel_token,
            ))
            .catch_unwind()
            .await;
            match result {
                Ok(res) => {
                    if let Err(e) = res {
                        tracing::error!("SSE loop error: {}", e);
                    }
                }
                Err(panic_err) => {
                    if let Some(msg) = panic_err.downcast_ref::<&str>() {
                        tracing::error!("SSE loop panicked: {}", msg);
                    } else if let Some(msg) = panic_err.downcast_ref::<String>() {
                        tracing::error!("SSE loop panicked: {}", msg);
                    } else {
                        tracing::error!("SSE loop panicked with unknown error");
                    }
                }
            }
        });

        let mut transport = Self {
            client,
            base_url: url,
            headers,
            oauth,
            endpoint_url: None,
            receiver: rx,
            pending_messages: VecDeque::new(),
        };
        transport
            .wait_for_endpoint(&wait_cancel_token, endpoint_timeout)
            .await?;
        Ok(transport)
    }

    async fn run_sse_loop(
        client: reqwest::Client,
        url: String,
        headers: HashMap<String, String>,
        tx: tokio::sync::mpsc::UnboundedSender<SseInbound>,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<()> {
        let response = with_bearer_header(
            apply_safe_custom_headers(
                with_default_mcp_http_headers(client.get(&url), false),
                &headers,
            ),
            &url,
            &headers,
        )
        .send()
        .await
        .with_context(|| {
            format!(
                "MCP SSE connect failed (transport=http url={})",
                mask_url_secrets(&url),
            )
        })?;
        let status = response.status();
        if !status.is_success() {
            let body_excerpt = bounded_body_excerpt(response, ERROR_BODY_PREVIEW_BYTES).await;
            anyhow::bail!(
                "MCP SSE rejected (transport=http url={} status={}): {}",
                mask_url_secrets(&url),
                status,
                body_excerpt,
            );
        }

        let mut stream = response.bytes_stream();
        use futures_util::StreamExt;
        let mut buffer = String::new();

        loop {
            if cancel_token.is_cancelled() {
                tracing::debug!("SSE loop cancelled");
                break;
            }
            let item = tokio::select! {
                _ = cancel_token.cancelled() => {
                    tracing::debug!("SSE loop shutting down");
                    break;
                }
                item = stream.next() => {
                    match item {
                        Some(i) => i,
                        None => break,
                    }
                }
            };
            let chunk = item?;
            let s = String::from_utf8_lossy(&chunk);
            buffer.push_str(&s);

            while let Some((pos, separator_len)) = find_sse_event_separator(&buffer) {
                let event_block = buffer[..pos].to_string();
                buffer = buffer[pos + separator_len..].to_string();

                let mut event_type = "message";
                let mut data = String::new();

                for line in event_block.lines() {
                    if let Some(value) = sse_field_value(line, "event:") {
                        event_type = value;
                    } else if let Some(value) = sse_field_value(line, "data:") {
                        if !data.is_empty() {
                            data.push('\n');
                        }
                        data.push_str(value);
                    }
                }

                match event_type {
                    "endpoint" => {
                        let _ = tx.send(SseInbound::Endpoint(data));
                    }
                    "message" if !data.trim().is_empty() => {
                        let _ = tx.send(SseInbound::Message(data.into_bytes()));
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    async fn wait_for_endpoint(
        &mut self,
        cancel_token: &tokio_util::sync::CancellationToken,
        endpoint_timeout: Duration,
    ) -> Result<()> {
        let timeout = tokio::time::sleep(endpoint_timeout);
        tokio::pin!(timeout);

        loop {
            let msg = tokio::select! {
                _ = cancel_token.cancelled() => {
                    anyhow::bail!("SSE transport cancelled before endpoint was discovered");
                }
                _ = &mut timeout => {
                    anyhow::bail!(
                        "SSE endpoint not received within {}ms",
                        endpoint_timeout.as_millis()
                    );
                }
                msg = self.receiver.recv() => {
                    msg.context("SSE transport closed before endpoint was discovered")?
                }
            };

            match msg {
                SseInbound::Endpoint(endpoint) => {
                    self.store_endpoint(&endpoint)?;
                    return Ok(());
                }
                SseInbound::Message(msg) => self.pending_messages.push_back(msg),
            }
        }
    }

    fn store_endpoint(&mut self, endpoint: &str) -> Result<()> {
        self.endpoint_url = Some(Self::resolve_endpoint_url(&self.base_url, endpoint)?);
        Ok(())
    }

    fn resolve_endpoint_url(base_url: &str, endpoint_url: &str) -> Result<String> {
        if endpoint_url.starts_with("http://") || endpoint_url.starts_with("https://") {
            return Ok(endpoint_url.to_string());
        }
        let base = reqwest::Url::parse(base_url)?;
        let joined = base.join(endpoint_url)?;
        Ok(joined.to_string())
    }
}

impl HttpTransport {
    fn new(
        client: reqwest::Client,
        url: String,
        headers: HashMap<String, String>,
        cancel_token: tokio_util::sync::CancellationToken,
        endpoint_timeout: Duration,
        oauth: Option<OAuthState>,
    ) -> Self {
        Self {
            mode: HttpTransportMode::Streamable(StreamableHttpTransport::new(
                client.clone(),
                url.clone(),
                headers.clone(),
                oauth.clone(),
            )),
            client,
            base_url: url,
            headers,
            oauth,
            cancel_token,
            endpoint_timeout,
        }
    }

    async fn switch_to_sse_and_send(&mut self, msg: Vec<u8>) -> Result<()> {
        let mut sse = SseTransport::connect(
            self.client.clone(),
            self.base_url.clone(),
            self.headers.clone(),
            self.cancel_token.clone(),
            self.endpoint_timeout,
            self.oauth.clone(),
        )
        .await?;
        sse.send(msg).await?;
        self.mode = HttpTransportMode::Sse(sse);
        Ok(())
    }

    /// Best-effort session-establishment GET preflight.
    ///
    /// Per the Streamable HTTP spec, the server may return an
    /// `Mcp-Session-Id` header on the `initialize` response (the normal
    /// path handled inside [`StreamableHttpTransport::send`] above).
    /// However some servers (e.g. Hindsight, #1629) **require** a session
    /// ID on every POST including `initialize`, creating a chicken-and-egg
    /// problem. For those servers we send a short-lived GET before the
    /// first POST: if the server returns a session ID in the GET response
    /// it will be captured by the header-reading code in
    /// [`StreamableHttpTransport::send`] just as if it came from a POST
    /// response.
    ///
    /// This is intentionally best-effort:
    /// * The GET uses a tight per-request inner timeout so it never
    ///   blocks connection startup for long.
    /// * If the server doesn't support GET (405, 404, …) we log a debug
    ///   line and move on — the `initialize` POST will proceed without a
    ///   session ID.
    /// * If the server opens an SSE stream in response (the GET from old
    ///   SSE transport), we read only the headers, then discard the body
    ///   so the SSE stream is torn down. The actual SSE path uses a
    ///   dedicated `SseTransport` and is triggered by the incompatible-
    ///   status fallback in [`HttpTransport::send`].
    async fn try_establish_session(&mut self) -> Result<()> {
        let transport = match &mut self.mode {
            HttpTransportMode::Streamable(t) => t,
            // Already on SSE — session is implicit via the long-lived GET.
            HttpTransportMode::Sse(_) => return Ok(()),
        };

        let request = with_bearer_header(
            apply_safe_custom_headers(
                with_default_mcp_http_headers(transport.client.get(&transport.url), false),
                &transport.headers,
            ),
            &transport.url,
            &transport.headers,
        );
        let response = tokio::time::timeout(Duration::from_secs(5), request.send())
            .await
            .map_err(|_| anyhow::anyhow!("GET timeout"))?
            .map_err(|e| anyhow::anyhow!("GET error: {e}"))?;

        // Capture session ID from the GET response so subsequent POSTs
        // (including `initialize`) can include it. This is the same
        // header-reading logic that would be hit inside
        // `StreamableHttpTransport::send` for POST responses, but since
        // the GET is sent before any POST we do it here directly.
        if let Some(sid) = response
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|v| v.to_str().ok())
            && transport.session_id.as_deref() != Some(sid)
        {
            tracing::debug!(target: "mcp", session_id = %sid, "captured MCP session ID via GET preflight");
            transport.session_id = Some(sid.to_string());
        }

        // We only care about the response headers — discard the body.
        // If the server opened an SSE stream in response (some servers
        // do this on GET), it will be torn down when response is dropped.
        drop(response);

        Ok(())
    }
}

#[async_trait::async_trait]
impl McpTransport for HttpTransport {
    async fn send(&mut self, msg: Vec<u8>) -> Result<()> {
        match &mut self.mode {
            HttpTransportMode::Streamable(transport) => match transport.send(msg.clone()).await {
                Ok(()) => Ok(()),
                Err(StreamableSendError::Incompatible(detail)) => {
                    tracing::debug!(
                        "MCP Streamable HTTP unavailable; falling back to SSE endpoint discovery: {}",
                        detail
                    );
                    self.switch_to_sse_and_send(msg).await
                }
                Err(StreamableSendError::StaleSession(detail)) => {
                    if let HttpTransportMode::Streamable(transport) = &mut self.mode {
                        tracing::debug!(
                            target: "mcp",
                            error = %detail,
                            "MCP Streamable HTTP session expired; clearing cached session ID"
                        );
                        transport.session_id = None;
                    }
                    Err(anyhow::anyhow!(
                        "MCP Streamable HTTP session expired; retry with a new session required ({detail})"
                    ))
                }
                Err(StreamableSendError::Other(err)) => Err(err),
            },
            HttpTransportMode::Sse(transport) => transport.send(msg).await,
        }
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        match &mut self.mode {
            HttpTransportMode::Streamable(transport) => transport.recv().await,
            HttpTransportMode::Sse(transport) => transport.recv().await,
        }
    }

    async fn shutdown(&mut self) {
        if let HttpTransportMode::Sse(transport) = &mut self.mode {
            transport.shutdown().await;
        }
    }
}

impl StreamableHttpTransport {
    fn new(
        client: reqwest::Client,
        url: String,
        headers: HashMap<String, String>,
        oauth: Option<OAuthState>,
    ) -> Self {
        Self {
            client,
            url,
            headers,
            oauth,
            pending_messages: VecDeque::new(),
            session_id: None,
        }
    }

    /// One 401-driven auth refresh: the Claude.ai manual-refresh seam first,
    /// then a stored OAuth token refresh. Returns whether the caller should
    /// retry the request.
    async fn refresh_auth_headers(&mut self, status: StatusCode) -> bool {
        if let Ok(true) = maybe_refresh_claude_headers(&self.url, status, &mut self.headers).await {
            return true;
        }
        if status == StatusCode::UNAUTHORIZED
            && let Some(oauth) = self.oauth.clone()
            && maybe_refresh_oauth_headers(&self.client, &mut self.headers, &oauth).await
        {
            return true;
        }
        false
    }

    async fn send(&mut self, msg: Vec<u8>) -> std::result::Result<(), StreamableSendError> {
        let response = self.send_once(msg.clone()).await?;
        let status = response.status();
        if !status.is_success() && self.refresh_auth_headers(status).await {
            let response = self.send_once(msg).await?;
            return self.handle_response(response).await;
        }
        self.handle_response(response).await
    }

    async fn send_once(
        &mut self,
        msg: Vec<u8>,
    ) -> std::result::Result<reqwest::Response, StreamableSendError> {
        // Apply user-configured custom headers after protocol framing so
        // reserved Accept / Content-Type overrides can be filtered out.
        let mut request = with_bearer_header(
            apply_safe_custom_headers(
                with_default_mcp_http_headers(self.client.post(&self.url), true),
                &self.headers,
            ),
            &self.url,
            &self.headers,
        );
        // Attach any previously captured session ID per the Streamable
        // HTTP spec so the server can correlate this request to the
        // existing session.
        if let Some(ref sid) = self.session_id {
            request = request.header("Mcp-Session-Id", sid.as_str());
        }
        request
            .body(msg)
            .send()
            .await
            .map_err(|err| StreamableSendError::Other(err.into()))
    }

    async fn handle_response(
        &mut self,
        response: reqwest::Response,
    ) -> std::result::Result<(), StreamableSendError> {
        let status = response.status();

        // Capture session ID from any response (2xx, 202, 4xx, …). The
        // server may return it on the `initialize` response or on a
        // best-effort GET preflight below.
        if let Some(sid) = response
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|v| v.to_str().ok())
            && self.session_id.as_deref() != Some(sid)
        {
            tracing::debug!(target: "mcp", session_id = %sid, "captured MCP session ID");
            self.session_id = Some(sid.to_string());
        }
        if status == StatusCode::ACCEPTED || status == StatusCode::NO_CONTENT {
            return Ok(());
        }

        if !status.is_success() {
            let body_excerpt = bounded_body_excerpt(response, ERROR_BODY_PREVIEW_BYTES).await;
            if self.session_id.is_some()
                && is_streamable_http_stale_session_status(status, &body_excerpt)
            {
                let detail = if status == StatusCode::NOT_FOUND
                    && body_has_jsonrpc_error_code(&body_excerpt, -32001)
                {
                    format!("status={status} jsonrpc_error=-32001 body={body_excerpt}")
                } else {
                    format!("status={status} body={body_excerpt}")
                };
                return Err(StreamableSendError::StaleSession(detail));
            }
            if is_streamable_http_incompatible_status(status) {
                return Err(StreamableSendError::Incompatible(format!(
                    "status={status} body={body_excerpt}"
                )));
            }
            return Err(StreamableSendError::Other(anyhow::anyhow!(
                "MCP Streamable HTTP rejected (transport=http url={} status={}): {}",
                mask_url_secrets(&self.url),
                status,
                body_excerpt,
            )));
        }

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = response
            .text()
            .await
            .map_err(|err| StreamableSendError::Other(err.into()))?;
        self.store_response_body(content_type.as_deref(), &body)
            .map_err(StreamableSendError::Other)
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        self.pending_messages
            .pop_front()
            .context("MCP Streamable HTTP response queue is empty")
    }

    fn store_response_body(&mut self, content_type: Option<&str>, body: &str) -> Result<()> {
        if body.trim().is_empty() {
            return Ok(());
        }

        let is_event_stream = content_type
            .map(|value| value.to_ascii_lowercase().contains("text/event-stream"))
            .unwrap_or(false)
            || body.trim_start().starts_with("event:")
            || body.trim_start().starts_with("data:");

        if is_event_stream {
            for msg in parse_sse_message_data(body) {
                self.pending_messages.push_back(msg);
            }
            return Ok(());
        }

        self.pending_messages.push_back(body.as_bytes().to_vec());
        Ok(())
    }
}

fn is_streamable_http_incompatible_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::NOT_FOUND
            | StatusCode::METHOD_NOT_ALLOWED
            | StatusCode::NOT_ACCEPTABLE
            | StatusCode::UNSUPPORTED_MEDIA_TYPE
            | StatusCode::NOT_IMPLEMENTED
    )
}

fn is_streamable_http_stale_session_status(status: StatusCode, body_excerpt: &str) -> bool {
    if status == StatusCode::NOT_FOUND {
        return true;
    }
    if status != StatusCode::BAD_REQUEST && status != StatusCode::UNAUTHORIZED {
        return false;
    }
    let body = body_excerpt.to_ascii_lowercase();
    body.contains("session") && (body.contains("expired") || body.contains("invalid"))
}

fn body_has_jsonrpc_error_code(body: &str, code: i64) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/code")
                .and_then(serde_json::Value::as_i64)
        })
        == Some(code)
}

fn is_mcp_auth_error_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("status=401")
        || lower.contains("status=403")
        || lower.contains(" 401")
        || lower.contains(" 403")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("needs-auth")
        || lower.contains("step-up")
        || lower.contains("step up")
}

fn is_mcp_auth_error(err: &anyhow::Error) -> bool {
    is_mcp_auth_error_text(&format!("{err:#}"))
}

fn is_mcp_stale_session_body(body: &str) -> bool {
    if body_has_jsonrpc_error_code(body, -32001) {
        return true;
    }
    let body = body.to_ascii_lowercase();
    body.contains("session") && (body.contains("expired") || body.contains("invalid"))
}

fn is_mcp_stale_session_error(err: &anyhow::Error) -> bool {
    let err = format!("{err:#}");
    err.contains("MCP Streamable HTTP session expired")
        || err.contains("MCP session expired")
        || err.contains("SSE transport closed")
        || err.contains("WebSocket transport closed")
        || is_mcp_stale_session_body(&err)
}

fn parse_sse_message_data(body: &str) -> Vec<Vec<u8>> {
    let normalized = body.replace("\r\n", "\n");
    let mut messages = Vec::new();

    for block in normalized.split("\n\n") {
        let mut event_type = "message";
        let mut data = String::new();

        for line in block.lines() {
            if let Some(value) = sse_field_value(line, "event:") {
                event_type = value;
            } else if let Some(value) = sse_field_value(line, "data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value);
            }
        }

        if event_type != "message" || data.trim().is_empty() {
            continue;
        }

        messages.push(data.trim().as_bytes().to_vec());
    }

    messages
}

fn find_sse_event_separator(buffer: &str) -> Option<(usize, usize)> {
    match (buffer.find("\n\n"), buffer.find("\r\n\r\n")) {
        (Some(lf), Some(crlf)) if crlf < lf => Some((crlf, 4)),
        (Some(lf), _) => Some((lf, 2)),
        (_, Some(crlf)) => Some((crlf, 4)),
        _ => None,
    }
}

fn sse_field_value<'a>(line: &'a str, field: &str) -> Option<&'a str> {
    let value = line.strip_prefix(field)?;
    Some(value.strip_prefix(' ').unwrap_or(value))
}

fn transport_kind(config: &McpServerConfig) -> Result<McpTransportKind> {
    McpTransportKind::parse(config.transport.as_deref())
}

#[cfg(test)]
fn is_legacy_sse_transport(config: &McpServerConfig) -> bool {
    transport_kind(config).is_ok_and(McpTransportKind::is_sse)
}

fn validate_mcp_transport(transport: Option<&str>) -> Result<()> {
    McpTransportKind::parse(transport).map(|_| ())
}

pub fn mcp_transport_label(transport: Option<&str>, has_url: bool) -> Result<&'static str> {
    if has_url {
        McpTransportKind::parse(transport).map(McpTransportKind::label)
    } else {
        Ok("stdio")
    }
}

pub fn validate_mcp_transport_option(transport: Option<&str>) -> Result<()> {
    validate_mcp_transport(transport)
}

fn response_id_matches(id: Option<&serde_json::Value>, expected_id: &str) -> bool {
    let Some(id) = id else {
        return false;
    };
    if id.as_str() == Some(expected_id) {
        return true;
    }
    id.as_u64()
        .map(|id| id.to_string() == expected_id)
        .unwrap_or(false)
}

#[async_trait::async_trait]
impl McpTransport for SseTransport {
    async fn send(&mut self, msg: Vec<u8>) -> Result<()> {
        let endpoint = self
            .endpoint_url
            .as_ref()
            .context("SSE endpoint not yet discovered")?;
        let endpoint_owned = endpoint.clone();
        let mut response = with_bearer_header(
            apply_safe_custom_headers(
                with_default_mcp_http_headers(self.client.post(&endpoint_owned), true),
                &self.headers,
            ),
            &endpoint_owned,
            &self.headers,
        )
        .body(msg.clone())
        .send()
        .await?;
        let mut status = response.status();
        if !status.is_success() {
            let refreshed = if let Ok(true) =
                maybe_refresh_claude_headers(&endpoint_owned, status, &mut self.headers).await
            {
                true
            } else if status == StatusCode::UNAUTHORIZED
                && let Some(oauth) = self.oauth.clone()
                && maybe_refresh_oauth_headers(&self.client, &mut self.headers, &oauth).await
            {
                true
            } else {
                false
            };
            if refreshed {
                response = with_bearer_header(
                    apply_safe_custom_headers(
                        with_default_mcp_http_headers(self.client.post(&endpoint_owned), true),
                        &self.headers,
                    ),
                    &endpoint_owned,
                    &self.headers,
                )
                .body(msg)
                .send()
                .await?;
                status = response.status();
            }
        }
        if !status.is_success() {
            let body_excerpt = bounded_body_excerpt(response, ERROR_BODY_PREVIEW_BYTES).await;
            if is_mcp_stale_session_body(&body_excerpt) {
                anyhow::bail!(
                    "MCP session expired (transport=sse endpoint={} status={}): {}",
                    mask_url_secrets(&endpoint_owned),
                    status,
                    body_excerpt
                );
            }
            anyhow::bail!(
                "MCP SSE POST rejected (transport=sse endpoint={} status={}): {}",
                mask_url_secrets(&endpoint_owned),
                status,
                body_excerpt
            );
        }
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        loop {
            if let Some(msg) = self.pending_messages.pop_front() {
                return Ok(msg);
            }

            match self.receiver.recv().await.context("SSE transport closed")? {
                SseInbound::Endpoint(endpoint) => {
                    self.store_endpoint(&endpoint)?;
                }
                SseInbound::Message(msg) => return Ok(msg),
            }
        }
    }
}

// === McpConnection - Async Connection Management ===

/// Manages a single async connection to an MCP server
pub struct McpConnection {
    name: String,
    transport: Box<dyn McpTransport>,
    tools: Vec<McpTool>,
    resources: Vec<McpResource>,
    resource_templates: Vec<McpResourceTemplate>,
    prompts: Vec<McpPrompt>,
    request_id: AtomicU64,
    state: ConnectionState,
    config: McpServerConfig,
    cancel_token: tokio_util::sync::CancellationToken,
}

impl McpConnection {
    /// Connect to an MCP server and initialize it.
    ///
    /// `network_policy` (added in v0.7.0 for #135) is consulted for HTTP/SSE
    /// transports only — STDIO transports are unaffected. Pass `None` to
    /// match pre-v0.7.0 permissive behavior.
    pub async fn connect_with_policy(
        name: String,
        config: McpServerConfig,
        global_timeouts: &McpTimeouts,
        network_policy: Option<&NetworkPolicyDecider>,
    ) -> Result<Self> {
        let connect_timeout_secs = config.effective_connect_timeout(global_timeouts);
        let cancel_token = tokio_util::sync::CancellationToken::new();

        let transport: Box<dyn McpTransport> = if let Some(url) = &config.url {
            // Per-domain network policy gate (#135). Only the HTTP/SSE transport
            // is gated; STDIO MCP servers run as local subprocesses and never
            // touch the network from this code path.
            if let Some(decider) = network_policy
                && let Some(host) = host_from_url(url)
            {
                match decider.evaluate(&host, "mcp") {
                    Decision::Allow => {}
                    Decision::Deny => {
                        anyhow::bail!(
                            "MCP server '{name}' connection to '{host}' blocked by network policy"
                        );
                    }
                    Decision::Prompt => {
                        anyhow::bail!(
                            "MCP server '{name}' connection to '{host}' requires approval; \
                             re-run after `/network allow {host}` or set network.default = \"allow\" in config"
                        );
                    }
                }
            }
            // Honor the standard `HTTP_PROXY` / `HTTPS_PROXY` (and their
            // lowercase equivalents) plus `NO_PROXY` env vars when
            // reaching MCP HTTP servers (#1408). Reqwest 0.13 does not
            // auto-detect these by default, so users behind corporate
            // proxies, on China-mainland connections routing through a
            // local Clash / Shadowsocks tunnel, etc. previously had MCP
            // HTTP traffic bypass the proxy entirely while every other
            // tool on the box (curl, npm, …) used it.
            let mut client_builder =
                reqwest::Client::builder().timeout(Duration::from_secs(connect_timeout_secs));
            let env_proxy_url = std::env::var("HTTPS_PROXY")
                .or_else(|_| std::env::var("https_proxy"))
                .or_else(|_| std::env::var("HTTP_PROXY"))
                .or_else(|_| std::env::var("http_proxy"))
                .ok()
                .filter(|s| !s.trim().is_empty());
            if let Some(proxy_url) = env_proxy_url {
                match reqwest::Proxy::all(&proxy_url) {
                    Ok(proxy) => {
                        let proxy = proxy.no_proxy(reqwest::NoProxy::from_env());
                        client_builder = client_builder.proxy(proxy);
                    }
                    Err(err) => {
                        // Redact userinfo (the `username[:password]@…`
                        // portion of the URL) before logging so an
                        // HTTPS_PROXY that embeds credentials
                        // (common in corporate setups) doesn't leak the
                        // password to the on-disk `~/.codesmith/logs/`.
                        let proxy_redacted = redact_proxy_userinfo(&proxy_url);
                        tracing::warn!(
                            target: "mcp",
                            ?err,
                            proxy = %proxy_redacted,
                            "ignoring malformed HTTP(S)_PROXY env var; MCP connection will bypass proxy"
                        );
                    }
                }
            }
            let client = client_builder.build()?;
            let kind = transport_kind(&config)?;
            // OAuth (#2190): resolve the stored token and inject it as the
            // Authorization header unless the user already configured one.
            // The HTTP transports additionally refresh it once on 401.
            let oauth_state = config.oauth.as_ref().map(|oauth| OAuthState {
                server: name.clone(),
                config: oauth.clone(),
            });
            let mut connect_headers = config.headers.clone();
            if let Some(state) = &oauth_state
                && !connect_headers
                    .keys()
                    .any(|key| key.eq_ignore_ascii_case("authorization"))
            {
                match crate::mcp_oauth::resolve_authorization_value(
                    &client,
                    &crate::mcp_oauth::default_secrets(),
                    &state.server,
                    &state.config,
                )
                .await
                {
                    Ok(Some(value)) => {
                        connect_headers.insert("Authorization".to_string(), value);
                    }
                    Ok(None) => tracing::warn!(
                        target: "mcp",
                        server = %name,
                        "oauth configured but no token stored; run `codesmith mcp auth {name}` to authorize"
                    ),
                    Err(err) => tracing::warn!(
                        target: "mcp",
                        server = %name,
                        error = %err,
                        "failed to resolve oauth token; connecting without Authorization"
                    ),
                }
            }
            if kind.is_websocket() {
                Box::new(
                    WebSocketTransport::connect(url.clone(), connect_headers.clone()).await?,
                )
            } else if kind.is_sse() {
                Box::new(
                    SseTransport::connect(
                        client,
                        url.clone(),
                        connect_headers.clone(),
                        cancel_token.clone(),
                        Duration::from_secs(connect_timeout_secs),
                        oauth_state.clone(),
                    )
                    .await?,
                )
            } else {
                let mut http = HttpTransport::new(
                    client,
                    url.clone(),
                    connect_headers.clone(),
                    cancel_token.clone(),
                    Duration::from_secs(connect_timeout_secs),
                    oauth_state.clone(),
                );
                // Best-effort session preflight for servers that require
                // a session ID on every POST including `initialize`
                // (e.g. Hindsight, #1629). Failures are non-fatal — the
                // `initialize` POST will proceed and may capture a session
                // ID from the response instead.
                if let Err(e) = http.try_establish_session().await {
                    tracing::debug!(
                        target: "mcp",
                        server = %name,
                        error = %e,
                        "session-establishment GET skipped; proceeding with POST initialize"
                    );
                }
                Box::new(http)
            }
        } else if let Some(command) = &config.command {
            let mut cmd = tokio::process::Command::new(command);
            cmd.args(&config.args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true);

            // MCP stdio servers are user-configured integrations. Use the
            // wider MCP allowlist so common Node/Python/proxy/CA-bundle
            // bootstrap variables (NVM_DIR, NODE_OPTIONS, NPM_CONFIG_*,
            // HTTP(S)_PROXY, …) reach the child. See `sanitized_mcp_env`
            // and #1244 for context.
            child_env::apply_to_tokio_command_mcp(&mut cmd, child_env::string_map_env(&config.env));

            let mut child = cmd.spawn().with_context(|| {
                let env_keys: Vec<&str> = config.env.keys().map(String::as_str).collect();
                format!(
                    "MCP stdio spawn failed (transport=stdio server={name} cmd={command:?} args={:?} env_keys={env_keys:?})",
                    config.args,
                )
            })?;

            let stdin = child.stdin.take().context("Failed to get MCP stdin")?;
            let stdout = child.stdout.take().context("Failed to get MCP stdout")?;
            let stderr = child.stderr.take().context("Failed to get MCP stderr")?;

            // Drain stderr into a bounded ring buffer so a crash mid-run
            // leaves diagnostic breadcrumbs instead of disappearing into
            // `Stdio::null`. The task exits naturally when the child closes
            // its stderr (kill_on_drop / exit / explicit shutdown).
            let stderr_tail = StderrTail::new();
            {
                let tail = Arc::clone(&stderr_tail);
                tokio::spawn(async move {
                    let mut lines = tokio::io::BufReader::new(stderr).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        tail.push(line).await;
                    }
                });
            }

            Box::new(StdioTransport {
                child,
                stdin,
                reader: tokio::io::BufReader::new(stdout),
                stderr_tail,
            })
        } else {
            anyhow::bail!("MCP server '{name}' config must have either 'command' or 'url'");
        };

        let mut conn = Self {
            name: name.clone(),
            transport,
            tools: Vec::new(),
            resources: Vec::new(),
            resource_templates: Vec::new(),
            prompts: Vec::new(),
            request_id: AtomicU64::new(1),
            state: ConnectionState::Connecting,
            config,
            cancel_token,
        };

        // Initialize with timeout
        tokio::time::timeout(Duration::from_secs(connect_timeout_secs), conn.initialize())
            .await
            .with_context(|| format!("MCP server '{name}' initialization timed out"))??;

        // Discover tools, resources, and prompts with timeout
        tokio::time::timeout(
            Duration::from_secs(connect_timeout_secs),
            conn.discover_all(),
        )
        .await
        .with_context(|| format!("MCP server '{name}' discovery timed out"))??;

        conn.state = ConnectionState::Ready;
        Ok(conn)
    }

    /// Send initialize request and wait for response
    async fn initialize(&mut self) -> Result<()> {
        let init_id = self.next_id();
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": &init_id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "clientInfo": {
                    "name": "codesmith-tui",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {
                    "tools": {},
                    "resources": {},
                    "prompts": {}
                }
            }
        }))
        .await?;

        self.recv(init_id).await?;

        // Send initialized notification (no id, no response expected)
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .await?;

        Ok(())
    }

    /// Discover tools, resources, and prompts
    async fn discover_all(&mut self) -> Result<()> {
        // We use join! to discover everything concurrently if possible,
        // but for now let's keep it sequential for simplicity in error handling
        self.discover_tools().await?;
        self.discover_resources().await?;
        self.discover_resource_templates().await?;
        self.discover_prompts().await?;
        Ok(())
    }

    /// Discover available tools from the MCP server
    async fn discover_tools(&mut self) -> Result<()> {
        let mut cursor: Option<String> = None;
        loop {
            let list_id = self.next_id();
            let params = match &cursor {
                Some(c) => serde_json::json!({ "cursor": c }),
                None => serde_json::json!({}),
            };
            self.send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": &list_id,
                "method": "tools/list",
                "params": params
            }))
            .await?;

            let response = self.recv(list_id).await?;
            let Some(result) = response.get("result") else {
                break;
            };

            if let Some(arr) = result.get("tools").and_then(|t| t.as_array()) {
                for item in arr {
                    match serde_json::from_value::<McpTool>(item.clone()) {
                        Ok(tool) => self.tools.push(tool),
                        Err(err) => {
                            // Skip individual malformed entries instead of
                            // dropping the whole page (#1410). The old
                            // `unwrap_or_default()` would silently throw
                            // away every tool when one was misshapen.
                            tracing::debug!(target: "mcp", ?err, "skipping malformed tool item");
                        }
                    }
                }
            }

            cursor = result
                .get("nextCursor")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        // Sort by tool name so the order the model sees doesn't depend on
        // server-side pagination ordering — keeps the prompt prefix stable
        // for cache-hit purposes (#1319).
        self.tools.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(())
    }

    /// Discover available resources from the MCP server
    async fn discover_resources(&mut self) -> Result<()> {
        let mut cursor: Option<String> = None;
        loop {
            let list_id = self.next_id();
            let params = match &cursor {
                Some(c) => serde_json::json!({ "cursor": c }),
                None => serde_json::json!({}),
            };
            self.send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": &list_id,
                "method": "resources/list",
                "params": params
            }))
            .await?;

            let response = self.recv(list_id).await?;
            let Some(result) = response.get("result") else {
                break;
            };

            if let Some(arr) = result.get("resources").and_then(|r| r.as_array()) {
                for item in arr {
                    match serde_json::from_value::<McpResource>(item.clone()) {
                        Ok(resource) => self.resources.push(resource),
                        Err(err) => {
                            tracing::debug!(target: "mcp", ?err, "skipping malformed resource item");
                        }
                    }
                }
            }

            cursor = result
                .get("nextCursor")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        Ok(())
    }

    /// Discover available resource templates from the MCP server
    async fn discover_resource_templates(&mut self) -> Result<()> {
        let mut cursor: Option<String> = None;
        loop {
            let list_id = self.next_id();
            let params = match &cursor {
                Some(c) => serde_json::json!({ "cursor": c }),
                None => serde_json::json!({}),
            };
            self.send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": &list_id,
                "method": "resources/templates/list",
                "params": params
            }))
            .await?;

            let response = self.recv(list_id).await?;
            let Some(result) = response.get("result") else {
                break;
            };

            let templates = result
                .get("resourceTemplates")
                .or_else(|| result.get("templates"))
                .or_else(|| result.get("resource_templates"));
            if let Some(arr) = templates.and_then(|t| t.as_array()) {
                for item in arr {
                    match serde_json::from_value::<McpResourceTemplate>(item.clone()) {
                        Ok(tmpl) => self.resource_templates.push(tmpl),
                        Err(err) => {
                            tracing::debug!(target: "mcp", ?err, "skipping malformed resource_template item");
                        }
                    }
                }
            }

            cursor = result
                .get("nextCursor")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        Ok(())
    }

    /// Discover available prompts from the MCP server
    async fn discover_prompts(&mut self) -> Result<()> {
        let mut cursor: Option<String> = None;
        loop {
            let list_id = self.next_id();
            let params = match &cursor {
                Some(c) => serde_json::json!({ "cursor": c }),
                None => serde_json::json!({}),
            };
            self.send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": &list_id,
                "method": "prompts/list",
                "params": params
            }))
            .await?;

            let response = self.recv(list_id).await?;
            let Some(result) = response.get("result") else {
                break;
            };

            if let Some(arr) = result.get("prompts").and_then(|p| p.as_array()) {
                for item in arr {
                    match serde_json::from_value::<McpPrompt>(item.clone()) {
                        Ok(prompt) => self.prompts.push(prompt),
                        Err(err) => {
                            tracing::debug!(target: "mcp", ?err, "skipping malformed prompt item");
                        }
                    }
                }
            }

            cursor = result
                .get("nextCursor")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        Ok(())
    }

    /// Call a tool on this MCP server
    pub async fn call_tool(
        &mut self,
        tool_name: &str,
        arguments: serde_json::Value,
        timeout_secs: u64,
    ) -> Result<serde_json::Value> {
        self.call_method(
            "tools/call",
            serde_json::json!({
                "name": tool_name,
                "arguments": arguments
            }),
            timeout_secs,
        )
        .await
    }

    /// Read a resource from this MCP server
    pub async fn read_resource(
        &mut self,
        uri: &str,
        timeout_secs: u64,
    ) -> Result<serde_json::Value> {
        self.call_method(
            "resources/read",
            serde_json::json!({
                "uri": uri
            }),
            timeout_secs,
        )
        .await
    }

    /// Get a prompt from this MCP server
    pub async fn get_prompt(
        &mut self,
        prompt_name: &str,
        arguments: serde_json::Value,
        timeout_secs: u64,
    ) -> Result<serde_json::Value> {
        self.call_method(
            "prompts/get",
            serde_json::json!({
                "name": prompt_name,
                "arguments": arguments
            }),
            timeout_secs,
        )
        .await
    }

    /// Generic method to call an MCP method
    async fn call_method(
        &mut self,
        method: &str,
        params: serde_json::Value,
        timeout_secs: u64,
    ) -> Result<serde_json::Value> {
        if self.state != ConnectionState::Ready {
            anyhow::bail!(
                "Failed to call MCP method '{}': connection '{}' is not ready",
                method,
                self.name
            );
        }

        let call_id = self.next_id();
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": &call_id,
            "method": method,
            "params": params
        }))
        .await?;

        let response = tokio::time::timeout(Duration::from_secs(timeout_secs), self.recv(call_id))
            .await
            .with_context(|| {
                format!(
                    "MCP method '{}' on server '{}' timed out after {}s",
                    method, self.name, timeout_secs
                )
            })??;

        if let Some(error) = response.get("error") {
            return Err(anyhow::anyhow!(
                "MCP error in '{}': {}",
                method,
                serde_json::to_string_pretty(error)?
            ));
        }

        Ok(response
            .get("result")
            .cloned()
            .unwrap_or(serde_json::json!(null)))
    }

    /// Get discovered tools
    pub fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    /// Get discovered resources
    pub fn resources(&self) -> &[McpResource] {
        &self.resources
    }

    /// Get discovered resource templates
    pub fn resource_templates(&self) -> &[McpResourceTemplate] {
        &self.resource_templates
    }

    /// Get discovered prompts
    pub fn prompts(&self) -> &[McpPrompt] {
        &self.prompts
    }

    /// Get server name
    #[allow(dead_code)] // Public API for MCP consumers
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Check if connection is ready
    pub fn is_ready(&self) -> bool {
        self.state == ConnectionState::Ready
    }

    /// Get server config
    pub fn config(&self) -> &McpServerConfig {
        &self.config
    }

    /// Get connection state
    #[allow(dead_code)] // Public API for MCP consumers
    pub fn state(&self) -> ConnectionState {
        self.state
    }

    fn next_id(&self) -> String {
        self.request_id.fetch_add(1, Ordering::SeqCst).to_string()
    }

    async fn send(&mut self, msg: serde_json::Value) -> Result<()> {
        let bytes = serde_json::to_vec(&msg).context("Failed to serialize MCP JSON-RPC message")?;
        self.transport.send(bytes).await
    }

    async fn recv(&mut self, expected_id: String) -> Result<serde_json::Value> {
        loop {
            let bytes = self.transport.recv().await.inspect_err(|_e| {
                self.state = ConnectionState::Disconnected;
            })?;
            let value: serde_json::Value = serde_json::from_slice(&bytes).with_context(|| {
                format!("Invalid MCP JSON-RPC message from server '{}'", self.name)
            })?;

            // Check if this is a response with the expected id. We emit
            // string IDs because some MCP gateways reject numeric JSON-RPC
            // IDs, but accept numeric echoes for compatibility with older
            // servers and tests.
            if response_id_matches(value.get("id"), &expected_id) {
                if let Some(error) = value.get("error")
                    && is_mcp_stale_session_body(&error.to_string())
                {
                    anyhow::bail!("MCP session expired: {error}");
                }
                return Ok(value);
            }
            // Skip notifications (no id) and responses with different ids
        }
    }

    /// Gracefully close the connection
    #[allow(dead_code)] // Public API for MCP consumers
    pub fn close(&mut self) {
        self.cancel_token.cancel();
        self.state = ConnectionState::Disconnected;
    }
}

impl Drop for McpConnection {
    fn drop(&mut self) {
        self.cancel_token.cancel();
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct McpAuthCache {
    #[serde(default)]
    servers: HashMap<String, McpAuthCacheEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct McpAuthCacheEntry {
    marked_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transport: Option<String>,
    reason: String,
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn default_mcp_auth_cache_path() -> Option<PathBuf> {
    crate::utils::effective_home_dir()
        .map(|home| home.join(".codesmith").join("mcp-needs-auth-cache.json"))
}

impl McpAuthCache {
    fn load() -> Self {
        let Some(path) = default_mcp_auth_cache_path() else {
            return Self::default();
        };
        fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    fn fresh_entry(&self, server: &str) -> Option<&McpAuthCacheEntry> {
        let entry = self.servers.get(server)?;
        let age = Duration::from_millis(now_millis().saturating_sub(entry.marked_at_ms));
        (age < MCP_AUTH_CACHE_TTL).then_some(entry)
    }

    fn mark(server: &str, config: &McpServerConfig, reason: &str) {
        let Some(path) = default_mcp_auth_cache_path() else {
            return;
        };
        let mut cache = Self::load();
        cache.servers.insert(
            server.to_string(),
            McpAuthCacheEntry {
                marked_at_ms: now_millis(),
                url: config.url.as_ref().map(|url| mask_url_secrets(url)),
                transport: config.transport.clone(),
                reason: reason.to_string(),
            },
        );
        if let Some(parent) = path.parent()
            && let Err(err) = fs::create_dir_all(parent)
        {
            tracing::warn!(target: "mcp", ?err, path = %parent.display(), "failed to create MCP auth cache directory");
            return;
        }
        match serde_json::to_string_pretty(&cache) {
            Ok(contents) => {
                if let Err(err) = write_atomic(&path, contents.as_bytes()) {
                    tracing::warn!(target: "mcp", ?err, path = %path.display(), "failed to write MCP auth cache");
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
                }
            }
            Err(err) => tracing::warn!(target: "mcp", ?err, "failed to serialize MCP auth cache"),
        }
    }
}

fn needs_auth_error(server: &str, entry: &McpAuthCacheEntry) -> anyhow::Error {
    anyhow::anyhow!(
        "MCP server '{server}' needs-auth (cached for {}s): {}",
        MCP_AUTH_CACHE_TTL.as_secs(),
        entry.reason
    )
}

// === McpPool - Connection Pool Management ===

/// Pool of MCP connections for reuse
pub struct McpPool {
    connections: HashMap<String, McpConnection>,
    config: McpConfig,
    network_policy: Option<NetworkPolicyDecider>,
    /// Source path the config was loaded from, when `from_config_path` was
    /// used. `None` for pools constructed directly via `new` (tests, ad-hoc
    /// snapshots). Drives the lazy-reload check (#1267 part 2): when the
    /// file's mtime moves, the pool re-reads the config and compares its
    /// content hash to decide whether to drop existing connections.
    config_source: Option<std::path::PathBuf>,
    /// 64-bit content hash of the active config (`hash_mcp_config`). Compared
    /// against the freshly-loaded config after an mtime change to skip
    /// reloading when the file was merely touched.
    config_hash: u64,
    /// Most recently observed mtime of `config_source`. Updated whenever the
    /// reload check runs (whether or not it triggered a reload).
    last_mtime: Option<std::time::SystemTime>,
}

fn connection_batch_size(env_name: &str, default: usize) -> usize {
    std::env::var(env_name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[derive(Clone)]
struct McpConnectTask {
    name: String,
    config: McpServerConfig,
}

async fn connect_task(
    task: McpConnectTask,
    timeouts: McpTimeouts,
    policy: Option<NetworkPolicyDecider>,
) -> (String, Result<McpConnection>) {
    let result = if task.config.url.is_some()
        && let Some(entry) = McpAuthCache::load().fresh_entry(&task.name)
    {
        Err(needs_auth_error(&task.name, entry))
    } else {
        let connected = McpConnection::connect_with_policy(
            task.name.clone(),
            task.config.clone(),
            &timeouts,
            policy.as_ref(),
        )
        .await;
        match connected {
            Ok(conn) => Ok(conn),
            Err(err) if task.config.url.is_some() && is_mcp_auth_error(&err) => {
                McpAuthCache::mark(&task.name, &task.config, &format!("{err:#}"));
                Err(needs_auth_error(
                    &task.name,
                    McpAuthCache::load()
                        .fresh_entry(&task.name)
                        .expect("auth cache entry just written"),
                ))
            }
            Err(err) => Err(err),
        }
    };
    (task.name, result)
}

async fn connect_tasks_batched(
    tasks: Vec<McpConnectTask>,
    limit: usize,
    timeouts: McpTimeouts,
    policy: Option<NetworkPolicyDecider>,
) -> Vec<(String, Result<McpConnection>)> {
    futures_util::stream::iter(tasks.into_iter().map(|task| {
        let policy = policy.clone();
        async move { connect_task(task, timeouts, policy).await }
    }))
    .buffer_unordered(limit)
    .collect()
    .await
}

impl McpPool {
    /// Create a new pool with the given configuration
    pub fn new(config: McpConfig) -> Self {
        let config_hash = hash_mcp_config(&config);
        Self {
            connections: HashMap::new(),
            config,
            network_policy: None,
            config_source: None,
            config_hash,
            last_mtime: None,
        }
    }

    /// Create a pool from a configuration file path
    pub fn from_config_path(path: &std::path::Path) -> Result<Self> {
        validate_mcp_config_path(path)?;
        let config = if path.exists() {
            let contents = fs::read_to_string(path)
                .with_context(|| format!("Failed to read MCP config: {}", path.display()))?;
            serde_json::from_str(&contents)
                .with_context(|| format!("Failed to parse MCP config: {}", path.display()))?
        } else {
            McpConfig::default()
        };
        let last_mtime = mcp_config_mtime(path);
        let mut pool = Self::new(config);
        pool.config_source = Some(path.to_path_buf());
        pool.last_mtime = last_mtime;
        Ok(pool)
    }

    /// Attach a per-domain network policy (#135). When set, HTTP/SSE
    /// transports are gated through it; STDIO transports are unaffected.
    pub fn with_network_policy(mut self, policy: NetworkPolicyDecider) -> Self {
        self.network_policy = Some(policy);
        self
    }

    /// If the source config file's mtime has changed since the last check,
    /// re-read it and (only when the content hash also changed) drop all
    /// existing connections so the next `get_or_connect` reattaches under
    /// the new config. No-op when the pool was constructed via [`McpPool::new`]
    /// (no source path), when stat fails, or when the file content is
    /// byte-identical to what we last loaded. Returns `Ok(true)` if any
    /// connections were dropped, `Ok(false)` otherwise.
    ///
    /// This is the lazy half of the auto-reload story for #1267: instead of a
    /// long-lived file watcher, the next tool invocation pays a single `stat`
    /// call (and only re-reads the file when the mtime moved). On networked
    /// or remote filesystems where mtime granularity is poor, the hash
    /// compare keeps us from churning connections on every check.
    pub async fn reload_if_config_changed(&mut self) -> Result<bool> {
        let Some(path) = self.config_source.clone() else {
            return Ok(false);
        };
        let current_mtime = match mcp_config_mtime(&path) {
            Some(m) => m,
            None => return Ok(false),
        };
        if Some(current_mtime) == self.last_mtime {
            return Ok(false);
        }
        // mtime moved — we owe a re-read.
        let new_config: McpConfig = if path.exists() {
            let contents = fs::read_to_string(&path)
                .with_context(|| format!("Failed to re-read MCP config: {}", path.display()))?;
            serde_json::from_str(&contents)
                .with_context(|| format!("Failed to re-parse MCP config: {}", path.display()))?
        } else {
            McpConfig::default()
        };
        let new_hash = hash_mcp_config(&new_config);
        // Always advance last_mtime so a touched-but-unchanged file doesn't
        // make us re-read on every subsequent call.
        self.last_mtime = Some(current_mtime);
        if new_hash == self.config_hash {
            return Ok(false);
        }
        // Real content change — drop all live connections so the next
        // get_or_connect picks up the new config (sandbox flags, env, args).
        self.connections.clear();
        self.config = new_config;
        self.config_hash = new_hash;
        Ok(true)
    }

    /// Get or create a connection to a server
    pub async fn get_or_connect(&mut self, server_name: &str) -> Result<&mut McpConnection> {
        // Lazy auto-reload (#1267 part 2): cheap mtime-then-hash check before
        // each connection lookup. Errors from the reload check (stat failure,
        // partial config parse) are swallowed here so a transient FS hiccup
        // can't take down the whole tool dispatch — the user still gets the
        // existing connection to respond to.
        let _ = self.reload_if_config_changed().await;

        let is_ready = self
            .connections
            .get(server_name)
            .map(|conn| conn.is_ready())
            .unwrap_or(false);
        if is_ready {
            return self
                .connections
                .get_mut(server_name)
                .ok_or_else(|| anyhow::anyhow!("MCP connection disappeared for {server_name}"));
        }

        self.connections.remove(server_name);

        let server_config = self
            .config
            .servers
            .get(server_name)
            .ok_or_else(|| anyhow::anyhow!("Failed to find MCP server: {server_name}"))?
            .clone();

        if !server_config.is_enabled() {
            anyhow::bail!("Failed to connect MCP server '{server_name}': server is disabled");
        }

        if server_config.url.is_some()
            && let Some(entry) = McpAuthCache::load().fresh_entry(server_name)
        {
            return Err(needs_auth_error(server_name, entry));
        }

        let connection = match McpConnection::connect_with_policy(
            server_name.to_string(),
            server_config.clone(),
            &self.config.timeouts,
            self.network_policy.as_ref(),
        )
        .await
        {
            Ok(connection) => connection,
            Err(err) if server_config.url.is_some() && is_mcp_auth_error(&err) => {
                McpAuthCache::mark(server_name, &server_config, &format!("{err:#}"));
                return Err(needs_auth_error(
                    server_name,
                    McpAuthCache::load()
                        .fresh_entry(server_name)
                        .expect("auth cache entry just written"),
                ));
            }
            Err(err) => return Err(err),
        };

        self.connections.insert(server_name.to_string(), connection);
        self.connections
            .get_mut(server_name)
            .ok_or_else(|| anyhow::anyhow!("Failed to store MCP connection for {server_name}"))
    }

    /// Connect to all enabled servers, returning errors for failed connections
    pub async fn connect_all(&mut self) -> Vec<(String, anyhow::Error)> {
        let _ = self.reload_if_config_changed().await;

        let mut local_tasks = Vec::new();
        let mut remote_tasks = Vec::new();
        for (name, config) in &self.config.servers {
            if !config.is_enabled() {
                continue;
            }
            if self
                .connections
                .get(name)
                .is_some_and(McpConnection::is_ready)
            {
                continue;
            }
            let task = McpConnectTask {
                name: name.clone(),
                config: config.clone(),
            };
            if config.url.is_some() {
                remote_tasks.push(task);
            } else {
                local_tasks.push(task);
            }
        }

        let local_limit = connection_batch_size(
            "MCP_SERVER_CONNECTION_BATCH_SIZE",
            MCP_LOCAL_CONNECTION_BATCH_SIZE,
        );
        let remote_limit = connection_batch_size(
            "MCP_REMOTE_SERVER_CONNECTION_BATCH_SIZE",
            MCP_REMOTE_CONNECTION_BATCH_SIZE,
        );

        let mut errors = Vec::new();
        let mut results = connect_tasks_batched(
            local_tasks,
            local_limit,
            self.config.timeouts,
            self.network_policy.clone(),
        )
        .await;
        results.extend(
            connect_tasks_batched(
                remote_tasks,
                remote_limit,
                self.config.timeouts,
                self.network_policy.clone(),
            )
            .await,
        );

        for (name, result) in results {
            match result {
                Ok(connection) => {
                    self.connections.insert(name, connection);
                }
                Err(err) => errors.push((name, err)),
            }
        }

        for (name, server_cfg) in &self.config.servers {
            if server_cfg.required
                && server_cfg.is_enabled()
                && !self
                    .connections
                    .get(name)
                    .is_some_and(McpConnection::is_ready)
            {
                errors.push((
                    name.clone(),
                    anyhow::anyhow!("required MCP server failed to initialize"),
                ));
            }
        }

        errors
    }

    fn is_ide_server(&self, server: &str, conn: &McpConnection) -> bool {
        server == "ide" && transport_kind(conn.config()).is_ok_and(McpTransportKind::is_ide)
    }

    fn should_expose_tool(&self, server: &str, conn: &McpConnection, tool: &McpTool) -> bool {
        if !conn.config().is_tool_enabled(&tool.name) {
            return false;
        }
        let model_name = qualify_mcp_tool_name(server, &tool.name);
        !self.is_ide_server(server, conn) || is_allowed_ide_tool(&model_name)
    }

    /// Get all discovered tools with server-prefixed names
    pub fn all_tools(&self) -> Vec<(String, &McpTool)> {
        let mut tools = Vec::new();
        for (server, conn) in &self.connections {
            for tool in conn.tools() {
                if !self.should_expose_tool(server, conn, tool) {
                    continue;
                }
                tools.push((qualify_mcp_tool_name(server, &tool.name), tool));
            }
        }
        // Sort by prefixed name so iteration order across servers is
        // deterministic for prefix-cache stability (#1319).
        tools.sort_by(|a, b| a.0.cmp(&b.0));
        tools
    }

    /// Get all discovered resources with server-prefixed names
    pub fn all_resources(&self) -> Vec<(String, &McpResource)> {
        let mut resources = Vec::new();
        for (server, conn) in &self.connections {
            for resource in conn.resources() {
                // Format: mcp_{server}_{resource_name}
                // Note: resource names might contain spaces, we should probably slugify them
                let safe_name = resource.name.replace(' ', "_").to_lowercase();
                resources.push((qualify_mcp_tool_name(server, &safe_name), resource));
            }
        }
        resources
    }

    /// Get all discovered resource templates with server-prefixed names
    #[allow(dead_code)] // Public API for MCP resource discovery
    pub fn all_resource_templates(&self) -> Vec<(String, &McpResourceTemplate)> {
        let mut templates = Vec::new();
        for (server, conn) in &self.connections {
            for template in conn.resource_templates() {
                let safe_name = template.name.replace(' ', "_").to_lowercase();
                templates.push((qualify_mcp_tool_name(server, &safe_name), template));
            }
        }
        templates
    }

    async fn list_resources(&mut self, server: Option<String>) -> Result<Vec<serde_json::Value>> {
        if let Some(server_name) = server {
            let conn = self.get_or_connect(&server_name).await?;
            let resources = conn
                .resources()
                .iter()
                .map(|resource| {
                    serde_json::json!({
                        "server": server_name.clone(),
                        "uri": resource.uri,
                        "name": resource.name,
                        "description": resource.description,
                        "mime_type": resource.mime_type,
                    })
                })
                .collect();
            return Ok(resources);
        }

        let _ = self.connect_all().await;
        let mut items = Vec::new();
        for (server, conn) in &self.connections {
            for resource in conn.resources() {
                items.push(serde_json::json!({
                    "server": server,
                    "uri": resource.uri,
                    "name": resource.name,
                    "description": resource.description,
                    "mime_type": resource.mime_type,
                }));
            }
        }
        Ok(items)
    }

    async fn list_resource_templates(
        &mut self,
        server: Option<String>,
    ) -> Result<Vec<serde_json::Value>> {
        if let Some(server_name) = server {
            let conn = self.get_or_connect(&server_name).await?;
            let templates = conn
                .resource_templates()
                .iter()
                .map(|template| {
                    serde_json::json!({
                        "server": server_name.clone(),
                        "uri_template": template.uri_template,
                        "name": template.name,
                        "description": template.description,
                        "mime_type": template.mime_type,
                    })
                })
                .collect();
            return Ok(templates);
        }

        let _ = self.connect_all().await;
        let mut items = Vec::new();
        for (server, conn) in &self.connections {
            for template in conn.resource_templates() {
                items.push(serde_json::json!({
                    "server": server,
                    "uri_template": template.uri_template,
                    "name": template.name,
                    "description": template.description,
                    "mime_type": template.mime_type,
                }));
            }
        }
        Ok(items)
    }

    /// Get all discovered prompts with server-prefixed names
    pub fn all_prompts(&self) -> Vec<(String, &McpPrompt)> {
        let mut prompts = Vec::new();
        for (server, conn) in &self.connections {
            for prompt in conn.prompts() {
                // Format: mcp_{server}_{prompt}
                prompts.push((qualify_mcp_tool_name(server, &prompt.name), prompt));
            }
        }
        prompts
    }

    /// Read a resource from a specific server
    pub async fn read_resource(
        &mut self,
        server_name: &str,
        uri: &str,
    ) -> Result<serde_json::Value> {
        let global_timeouts = self.config.timeouts;
        let conn = self.get_or_connect(server_name).await?;
        let timeout = conn.config().effective_read_timeout(&global_timeouts);
        conn.read_resource(uri, timeout).await
    }

    /// Get a prompt from a specific server
    pub async fn get_prompt(
        &mut self,
        server_name: &str,
        prompt_name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let global_timeouts = self.config.timeouts;
        let conn = self.get_or_connect(server_name).await?;
        let timeout = conn.config().effective_execute_timeout(&global_timeouts);
        conn.get_prompt(prompt_name, arguments, timeout).await
    }

    /// Parse a prefixed name into (server_name, tool_name)
    fn parse_prefixed_name<'a>(&self, prefixed_name: &'a str) -> Result<(&'a str, &'a str)> {
        parse_mcp_tool_name(prefixed_name)
    }

    /// Convert discovered tools to API Tool format
    pub fn to_api_tools(&self) -> Vec<crate::models::Tool> {
        let mut api_tools = Vec::new();

        // Add regular tools
        for (name, tool) in self.all_tools() {
            api_tools.push(crate::models::Tool {
                tool_type: None,
                name,
                description: truncate_mcp_description(
                    tool.description.as_deref().unwrap_or_default(),
                ),
                input_schema: tool.input_schema.clone(),
                output_schema: None,
                allowed_callers: Some(vec!["direct".to_string()]),
                defer_loading: Some(false),
                input_examples: None,
                strict: None,
                cache_control: None,
            });
        }

        if !self.config.servers.is_empty() {
            api_tools.push(crate::models::Tool {
                tool_type: None,
                name: "list_mcp_resources".to_string(),
                description: "List available MCP resources across servers (optionally filtered by server).".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "server": { "type": "string", "description": "Optional MCP server name to filter by" }
                    }
                }),
                output_schema: None,
                allowed_callers: Some(vec!["direct".to_string()]),
                defer_loading: Some(false),
                input_examples: None,
                strict: None,
                cache_control: None,
            });
            api_tools.push(crate::models::Tool {
                tool_type: None,
                name: "list_mcp_resource_templates".to_string(),
                description: "List available MCP resource templates across servers (optionally filtered by server).".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "server": { "type": "string", "description": "Optional MCP server name to filter by" }
                    }
                }),
                output_schema: None,
                allowed_callers: Some(vec!["direct".to_string()]),
                defer_loading: Some(false),
                input_examples: None,
                strict: None,
                cache_control: None,
            });
        }

        // Add resource reading tools if resources exist
        let resources = self.all_resources();
        if !resources.is_empty() {
            api_tools.push(crate::models::Tool {
                tool_type: None,
                name: "mcp_read_resource".to_string(),
                description: "Read a resource from an MCP server using its URI".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "server": { "type": "string", "description": "The name of the MCP server" },
                        "uri": { "type": "string", "description": "The URI of the resource to read" }
                    },
                    "required": ["server", "uri"]
                }),
                output_schema: None,
                allowed_callers: Some(vec!["direct".to_string()]),
                defer_loading: Some(false),
                input_examples: None,
                strict: None,
                cache_control: None,
            });
            api_tools.push(crate::models::Tool {
                tool_type: None,
                name: "read_mcp_resource".to_string(),
                description: "Alias for mcp_read_resource.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "server": { "type": "string", "description": "The name of the MCP server" },
                        "uri": { "type": "string", "description": "The URI of the resource to read" }
                    },
                    "required": ["server", "uri"]
                }),
                output_schema: None,
                allowed_callers: Some(vec!["direct".to_string()]),
                defer_loading: Some(false),
                input_examples: None,
                strict: None,
                cache_control: None,
            });
        }

        // Add prompt getting tools if prompts exist
        let prompts = self.all_prompts();
        if !prompts.is_empty() {
            api_tools.push(crate::models::Tool {
                tool_type: None,
                name: "mcp_get_prompt".to_string(),
                description: "Get a prompt from an MCP server".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "server": { "type": "string", "description": "The name of the MCP server" },
                        "name": { "type": "string", "description": "The name of the prompt" },
                        "arguments": {
                            "type": "object",
                            "description": "Optional arguments for the prompt",
                            "additionalProperties": { "type": "string" }
                        }
                    },
                    "required": ["server", "name"]
                }),
                output_schema: None,
                allowed_callers: Some(vec!["direct".to_string()]),
                defer_loading: Some(false),
                input_examples: None,
                strict: None,
                cache_control: None,
            });
        }

        // Sort by name for prefix-cache stability — the tool block sent to
        // the model needs to be deterministic across runs (#1319).
        api_tools.sort_by(|a, b| a.name.cmp(&b.name));
        api_tools
    }

    /// Call a tool by its prefixed name (mcp__{server}__{tool}; legacy mcp_{server}_{tool} accepted)
    pub async fn call_tool(
        &mut self,
        prefixed_name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value> {
        // Sanitize tool arguments against Unicode hidden-character attacks
        // (HackerOne #3086545 — invisible Tag/zero-width chars that the model
        // still processes). Applied once here at the chokepoint so it covers
        // both `conn.call_tool` forwards below and the pseudo-tool readers.
        let arguments = crate::sanitization::recursively_sanitize_unicode(arguments);
        if prefixed_name == "list_mcp_resources" {
            let server = arguments
                .get("server")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let resources = self.list_resources(server).await?;
            return Ok(serde_json::json!({ "resources": resources }));
        }

        if prefixed_name == "list_mcp_resource_templates" {
            let server = arguments
                .get("server")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let templates = self.list_resource_templates(server).await?;
            return Ok(serde_json::json!({ "templates": templates }));
        }

        if prefixed_name == "mcp_read_resource" {
            let server_name = arguments
                .get("server")
                .and_then(|v| v.as_str())
                .context("Missing 'server' argument")?;
            let uri = arguments
                .get("uri")
                .and_then(|v| v.as_str())
                .context("Missing 'uri' argument")?;
            return self.read_resource(server_name, uri).await;
        }

        if prefixed_name == "read_mcp_resource" {
            let server_name = arguments
                .get("server")
                .and_then(|v| v.as_str())
                .context("Missing 'server' argument")?;
            let uri = arguments
                .get("uri")
                .and_then(|v| v.as_str())
                .context("Missing 'uri' argument")?;
            return self.read_resource(server_name, uri).await;
        }

        if prefixed_name == "mcp_get_prompt" {
            let server_name = arguments
                .get("server")
                .and_then(|v| v.as_str())
                .context("Missing 'server' argument")?;
            let name = arguments
                .get("name")
                .and_then(|v| v.as_str())
                .context("Missing 'name' argument")?;
            let args = arguments
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            return self.get_prompt(server_name, name, args).await;
        }

        let (server_name, tool_name) = self.parse_prefixed_name(prefixed_name)?;
        let server_config_for_auth = self
            .config
            .servers
            .get(server_name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Failed to find MCP server: {server_name}"))?;
        if server_config_for_auth.url.is_some()
            && let Some(entry) = McpAuthCache::load().fresh_entry(server_name)
        {
            return Err(needs_auth_error(server_name, entry));
        }
        // Copy the global timeouts to avoid borrow conflict
        let global_timeouts = self.config.timeouts;
        let conn = self.get_or_connect(server_name).await?;
        if !conn.config().is_tool_enabled(tool_name) {
            anyhow::bail!("MCP tool '{tool_name}' is disabled for server '{server_name}'");
        }
        let timeout = conn.config().effective_execute_timeout(&global_timeouts);
        match conn.call_tool(tool_name, arguments.clone(), timeout).await {
            Ok(result) => Ok(result),
            Err(err) if is_mcp_stale_session_error(&err) => {
                tracing::debug!(
                    target: "mcp",
                    server = server_name,
                    tool = tool_name,
                    error = %err,
                    "retrying MCP tool call after stale session"
                );
                self.connections.remove(server_name);
                let conn = self.get_or_connect(server_name).await?;
                if !conn.config().is_tool_enabled(tool_name) {
                    anyhow::bail!("MCP tool '{tool_name}' is disabled for server '{server_name}'");
                }
                let timeout = conn.config().effective_execute_timeout(&global_timeouts);
                conn.call_tool(tool_name, arguments, timeout).await
            }
            Err(err) if is_mcp_auth_error(&err) && server_config_for_auth.url.is_some() => {
                McpAuthCache::mark(server_name, &server_config_for_auth, &format!("{err:#}"));
                Err(needs_auth_error(
                    server_name,
                    McpAuthCache::load()
                        .fresh_entry(server_name)
                        .expect("auth cache entry just written"),
                ))
            }
            Err(err) => Err(err),
        }
    }

    /// Get list of configured server names
    #[allow(dead_code)] // Public API for MCP consumers
    pub fn server_names(&self) -> Vec<&str> {
        self.config
            .servers
            .keys()
            .map(std::string::String::as_str)
            .collect()
    }

    /// Get list of connected server names
    pub fn connected_servers(&self) -> Vec<&str> {
        self.connections
            .iter()
            .filter(|(_, c)| c.is_ready())
            .map(|(n, _)| n.as_str())
            .collect()
    }

    /// Disconnect all connections
    #[allow(dead_code)] // Public API for MCP lifecycle management
    pub fn disconnect_all(&mut self) {
        self.connections.clear();
    }

    /// Graceful shutdown of every connection in the pool: send SIGTERM to
    /// each stdio child and give them a short grace period before drop
    /// fires SIGKILL. Whalescale#420.
    ///
    /// Call from the TUI exit path *before* dropping the pool to give
    /// MCP servers a chance to flush state. The fallback Drop on
    /// `StdioTransport` still sends SIGTERM if this never runs, so even
    /// abnormal exits avoid leaking PIDs without a signal.
    #[allow(dead_code)] // Wired in by callers that want graceful shutdown
    pub async fn shutdown_all(&mut self) {
        let names: Vec<String> = self.connections.keys().cloned().collect();
        for name in names {
            if let Some(conn) = self.connections.get_mut(&name) {
                conn.transport.shutdown().await;
            }
        }
        self.connections.clear();
    }

    /// Get the underlying configuration
    #[allow(dead_code)] // Public API for MCP consumers
    pub fn config(&self) -> &McpConfig {
        &self.config
    }

    /// Check if a tool name is an MCP tool
    pub fn is_mcp_tool(name: &str) -> bool {
        name.starts_with("mcp__")
            || (name.starts_with("mcp_") && parse_mcp_tool_name(name).is_ok())
            || matches!(
                name,
                "list_mcp_resources"
                    | "list_mcp_resource_templates"
                    | "mcp_read_resource"
                    | "read_mcp_resource"
                    | "mcp_get_prompt"
            )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpWriteStatus {
    Created,
    Overwritten,
    SkippedExists,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpDiscoveredItem {
    pub name: String,
    pub model_name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerSnapshot {
    pub name: String,
    pub enabled: bool,
    pub required: bool,
    pub transport: String,
    pub command_or_url: String,
    pub connect_timeout: u64,
    pub execute_timeout: u64,
    pub read_timeout: u64,
    pub connected: bool,
    pub error: Option<String>,
    pub tools: Vec<McpDiscoveredItem>,
    pub resources: Vec<McpDiscoveredItem>,
    pub prompts: Vec<McpDiscoveredItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpManagerSnapshot {
    pub config_path: std::path::PathBuf,
    pub config_exists: bool,
    pub restart_required: bool,
    pub servers: Vec<McpServerSnapshot>,
}

pub fn load_config(path: &Path) -> Result<McpConfig> {
    validate_mcp_config_path(path)?;
    if !path.exists() {
        return Ok(McpConfig::default());
    }
    let contents = fs::read_to_string(path)
        .with_context(|| format!("Failed to read MCP config {}", path.display()))?;
    serde_json::from_str(&contents)
        .with_context(|| format!("Failed to parse MCP config {}", path.display()))
}

/// 64-bit content hash of an [`McpConfig`]. Used by [`McpPool`] to decide
/// whether a freshly-read config differs from the one currently driving the
/// live connections. Hashing the JSON serialization avoids forcing every
/// nested config type to derive `Hash` (the timeouts struct, network policy
/// stubs, etc.). The hash is stable across runs of the same Rust toolchain
/// for byte-identical input.
fn hash_mcp_config(config: &McpConfig) -> u64 {
    use std::hash::{Hash, Hasher};
    let bytes = serde_json::to_vec(config).unwrap_or_default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// Best-effort fetch of the MCP config file's last-modified time. Returns
/// `None` when the file is missing, when stat fails, when the platform
/// doesn't expose mtime, or when the path fails the same allow-list check
/// that `load_config` / `save_config` apply. The lazy-reload check in
/// `McpPool::get_or_connect` treats `None` as "skip the check this turn",
/// so a rejected path simply degrades to "no auto-reload" rather than an
/// error path. Callers already validate via `validate_mcp_config_path` at
/// construction time; the redundant validation here keeps this helper
/// safe-by-construction for any future caller and ties the validation to
/// the call site rather than relying on cross-function reasoning.
fn mcp_config_mtime(path: &Path) -> Option<std::time::SystemTime> {
    validate_mcp_config_path(path).ok()?;
    fs::metadata(path).ok()?.modified().ok()
}

pub fn save_config(path: &Path, cfg: &McpConfig) -> Result<()> {
    validate_mcp_config_path(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("Failed to create MCP config directory {}", parent.display())
        })?;
    }
    let rendered = serde_json::to_string_pretty(cfg).context("Failed to serialize MCP config")?;
    write_atomic(path, rendered.as_bytes())
        .with_context(|| format!("Failed to write MCP config {}", path.display()))?;
    Ok(())
}

fn mcp_template_json() -> Result<String> {
    let mut cfg = McpConfig::default();
    cfg.servers.insert(
        "example".to_string(),
        McpServerConfig {
            command: Some("node".to_string()),
            args: vec!["./path/to/your-mcp-server.js".to_string()],
            env: HashMap::new(),
            url: None,
            transport: None,
            connect_timeout: None,
            execute_timeout: None,
            read_timeout: None,
            disabled: true,
            enabled: true,
            required: false,
            enabled_tools: Vec::new(),
            disabled_tools: Vec::new(),
            headers: HashMap::new(),
        },
    );
    serde_json::to_string_pretty(&cfg).context("Failed to render MCP template JSON")
}

pub fn init_config(path: &Path, force: bool) -> Result<McpWriteStatus> {
    if path.exists() && !force {
        return Ok(McpWriteStatus::SkippedExists);
    }
    let status = if path.exists() {
        McpWriteStatus::Overwritten
    } else {
        McpWriteStatus::Created
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("Failed to create MCP config directory {}", parent.display())
        })?;
    }
    let template = mcp_template_json()?;
    write_atomic(path, template.as_bytes())
        .with_context(|| format!("Failed to write MCP config {}", path.display()))?;
    Ok(status)
}

pub fn add_server_config(
    path: &Path,
    name: String,
    command: Option<String>,
    url: Option<String>,
    args: Vec<String>,
    transport: Option<String>,
) -> Result<()> {
    if command.is_none() && url.is_none() {
        anyhow::bail!("Provide either a command or URL for MCP server '{name}'.");
    }
    validate_mcp_transport(transport.as_deref())?;
    let mut cfg = load_config(path)?;
    cfg.servers.insert(
        name,
        McpServerConfig {
            command,
            args,
            env: HashMap::new(),
            url,
            transport,
            connect_timeout: None,
            execute_timeout: None,
            read_timeout: None,
            disabled: false,
            enabled: true,
            required: false,
            enabled_tools: Vec::new(),
            disabled_tools: Vec::new(),
            headers: HashMap::new(),
        },
    );
    save_config(path, &cfg)
}

pub fn remove_server_config(path: &Path, name: &str) -> Result<()> {
    let mut cfg = load_config(path)?;
    if cfg.servers.remove(name).is_none() {
        anyhow::bail!("MCP server '{name}' not found");
    }
    save_config(path, &cfg)
}

pub fn set_server_enabled(path: &Path, name: &str, enabled: bool) -> Result<()> {
    let mut cfg = load_config(path)?;
    let server = cfg
        .servers
        .get_mut(name)
        .ok_or_else(|| anyhow::anyhow!("MCP server '{name}' not found"))?;
    server.enabled = enabled;
    server.disabled = !enabled;
    save_config(path, &cfg)
}

pub fn manager_snapshot_from_config(
    path: &Path,
    restart_required: bool,
) -> Result<McpManagerSnapshot> {
    let cfg = load_config(path)?;
    Ok(snapshot_from_config(
        path,
        path.exists(),
        restart_required,
        &cfg,
        None,
    ))
}

pub async fn discover_manager_snapshot(
    path: &Path,
    network_policy: Option<NetworkPolicyDecider>,
    restart_required: bool,
) -> Result<McpManagerSnapshot> {
    let cfg = load_config(path)?;
    let mut pool = McpPool::new(cfg.clone());
    if let Some(policy) = network_policy {
        pool = pool.with_network_policy(policy);
    }
    let errors = pool
        .connect_all()
        .await
        .into_iter()
        .map(|(name, err)| (name, format!("{err:#}")))
        .collect::<HashMap<_, _>>();
    Ok(snapshot_from_config(
        path,
        path.exists(),
        restart_required,
        &cfg,
        Some((&pool, &errors)),
    ))
}

fn snapshot_from_config(
    path: &Path,
    config_exists: bool,
    restart_required: bool,
    cfg: &McpConfig,
    discovery: Option<(&McpPool, &HashMap<String, String>)>,
) -> McpManagerSnapshot {
    let mut servers = cfg
        .servers
        .iter()
        .map(|(name, server)| {
            let transport = if server.url.is_some() {
                transport_kind(server)
                    .map(McpTransportKind::label)
                    .unwrap_or("invalid")
            } else {
                "stdio"
            };
            let command_or_url = server.url.clone().unwrap_or_else(|| {
                let mut command = server
                    .command
                    .clone()
                    .unwrap_or_else(|| "(missing)".to_string());
                if !server.args.is_empty() {
                    command.push(' ');
                    command.push_str(&server.args.join(" "));
                }
                command
            });
            let mut snapshot = McpServerSnapshot {
                name: name.clone(),
                enabled: server.is_enabled(),
                required: server.required,
                transport: transport.to_string(),
                command_or_url,
                connect_timeout: server.effective_connect_timeout(&cfg.timeouts),
                execute_timeout: server.effective_execute_timeout(&cfg.timeouts),
                read_timeout: server.effective_read_timeout(&cfg.timeouts),
                connected: false,
                error: if server.is_enabled() {
                    None
                } else {
                    Some("disabled".to_string())
                },
                tools: Vec::new(),
                resources: Vec::new(),
                prompts: Vec::new(),
            };

            if let Some((pool, errors)) = discovery {
                if let Some(error) = errors.get(name) {
                    snapshot.error = Some(error.clone());
                }
                if let Some(conn) = pool.connections.get(name) {
                    snapshot.connected = conn.is_ready();
                    snapshot.tools = conn
                        .tools()
                        .iter()
                        .filter(|tool| pool.should_expose_tool(name, conn, tool))
                        .map(|tool| McpDiscoveredItem {
                            name: tool.name.clone(),
                            model_name: qualify_mcp_tool_name(name, &tool.name),
                            description: tool.description.as_deref().map(truncate_mcp_description),
                        })
                        .collect();
                    snapshot.resources =
                        conn.resources()
                            .iter()
                            .map(|resource| McpDiscoveredItem {
                                name: resource.name.clone(),
                                model_name: qualify_mcp_tool_name(
                                    name,
                                    &resource.name.replace(' ', "_").to_lowercase(),
                                ),
                                description: resource.description.clone(),
                            })
                            .chain(conn.resource_templates().iter().map(|template| {
                                McpDiscoveredItem {
                                    name: template.name.clone(),
                                    model_name: qualify_mcp_tool_name(
                                        name,
                                        &template.name.replace(' ', "_").to_lowercase(),
                                    ),
                                    description: template.description.clone(),
                                }
                            }))
                            .collect();
                    snapshot.prompts = conn
                        .prompts()
                        .iter()
                        .map(|prompt| McpDiscoveredItem {
                            name: prompt.name.clone(),
                            model_name: qualify_mcp_tool_name(name, &prompt.name),
                            description: prompt.description.clone(),
                        })
                        .collect();
                }
            }

            snapshot
        })
        .collect::<Vec<_>>();
    servers.sort_by(|a, b| a.name.cmp(&b.name));
    McpManagerSnapshot {
        config_path: path.to_path_buf(),
        config_exists,
        restart_required,
        servers,
    }
}

// === Helper Functions ===

/// Format MCP tool result for display
#[allow(dead_code)] // Will be used when MCP tool results are displayed in TUI
pub fn format_tool_result(result: &serde_json::Value) -> String {
    let is_error = result
        .get("isError")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let content = result
        .get("content")
        .and_then(|v| v.as_array())
        .map_or_else(
            || serde_json::to_string_pretty(result).unwrap_or_default(),
            |arr| {
                arr.iter()
                    .filter_map(|item| match item.get("type")?.as_str()? {
                        "text" => item.get("text")?.as_str().map(String::from),
                        other => Some(format!("[{other} content]")),
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            },
        );

    if is_error {
        format!("Error: {content}")
    } else {
        content
    }
}

// === Unit Tests ===

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    use std::sync::{Arc, Mutex, OnceLock};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    async fn lock_mcp_loopback_tests() -> tokio::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await
    }

    #[test]
    fn truncate_mcp_description_is_utf8_safe_and_limited() {
        let description = "界".repeat(MAX_MCP_DESCRIPTION_LENGTH + 5);
        let truncated = truncate_mcp_description(&description);
        assert_eq!(truncated.chars().count(), MAX_MCP_DESCRIPTION_LENGTH);
        assert!(truncated.chars().all(|ch| ch == '界'));
    }

    #[test]
    fn mcp_transport_kind_accepts_url_transport_aliases() {
        assert_eq!(
            McpTransportKind::parse(None).unwrap(),
            McpTransportKind::StreamableHttp
        );
        assert_eq!(
            McpTransportKind::parse(Some("http")).unwrap(),
            McpTransportKind::StreamableHttp
        );
        assert_eq!(
            McpTransportKind::parse(Some("streamable")).unwrap(),
            McpTransportKind::StreamableHttp
        );
        assert_eq!(
            McpTransportKind::parse(Some("streamable-http")).unwrap(),
            McpTransportKind::StreamableHttp
        );
        assert_eq!(
            McpTransportKind::parse(Some("sse")).unwrap(),
            McpTransportKind::Sse
        );
        assert_eq!(
            McpTransportKind::parse(Some("sse-ide")).unwrap(),
            McpTransportKind::SseIde
        );
        assert_eq!(
            McpTransportKind::parse(Some("ws")).unwrap(),
            McpTransportKind::WebSocket
        );
        assert_eq!(
            McpTransportKind::parse(Some("websocket")).unwrap(),
            McpTransportKind::WebSocket
        );
        assert_eq!(
            McpTransportKind::parse(Some("ws-ide")).unwrap(),
            McpTransportKind::WebSocketIde
        );
        assert_eq!(
            McpTransportKind::parse(Some("websocket-ide")).unwrap(),
            McpTransportKind::WebSocketIde
        );
        assert!(McpTransportKind::parse(Some("ftp")).is_err());
    }

    #[test]
    fn mcp_transport_labels_match_configured_transport() {
        assert_eq!(mcp_transport_label(None, false).unwrap(), "stdio");
        assert_eq!(mcp_transport_label(None, true).unwrap(), "http/sse");
        assert_eq!(mcp_transport_label(Some("sse"), true).unwrap(), "sse");
        assert_eq!(
            mcp_transport_label(Some("sse-ide"), true).unwrap(),
            "sse-ide"
        );
        assert_eq!(mcp_transport_label(Some("ws"), true).unwrap(), "ws");
        assert_eq!(mcp_transport_label(Some("ws-ide"), true).unwrap(), "ws-ide");
    }

    #[test]
    fn websocket_headers_reject_protocol_overrides_and_add_claude_bearer() {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("CODESMITH_MCP_CLAUDE_ACCESS_TOKEN");
        unsafe {
            std::env::set_var("CODESMITH_MCP_CLAUDE_ACCESS_TOKEN", "test-token");
        }

        let mut headers = HashMap::new();
        headers.insert("X-Custom".to_string(), "ok".to_string());
        headers.insert("Sec-WebSocket-Key".to_string(), "bad".to_string());
        let request = "wss://claude.ai/api/mcp".into_client_request().unwrap();
        let request = apply_safe_websocket_headers(request, &headers);
        let request = with_websocket_bearer_header(request, "wss://claude.ai/api/mcp", &headers);

        assert_eq!(
            request
                .headers()
                .get("x-custom")
                .and_then(|v| v.to_str().ok()),
            Some("ok")
        );
        assert_ne!(
            request
                .headers()
                .get("sec-websocket-key")
                .and_then(|v| v.to_str().ok()),
            Some("bad")
        );
        assert_eq!(
            request
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok()),
            Some("Bearer test-token")
        );

        unsafe {
            match previous {
                Some(value) => std::env::set_var("CODESMITH_MCP_CLAUDE_ACCESS_TOKEN", value),
                None => std::env::remove_var("CODESMITH_MCP_CLAUDE_ACCESS_TOKEN"),
            }
        }
    }

    #[test]
    fn test_mcp_config_defaults() {
        let config = McpConfig::default();
        assert_eq!(config.timeouts.connect_timeout, 10);
        assert_eq!(config.timeouts.execute_timeout, 60);
        assert_eq!(config.timeouts.read_timeout, 120);
        assert!(config.servers.is_empty());
    }

    #[test]
    fn test_mcp_config_parse() {
        let json = r#"{
            "timeouts": {
                "connect_timeout": 15,
                "execute_timeout": 90
            },
            "servers": {
                "test": {
                    "command": "node",
                    "args": ["server.js"],
                    "env": {"FOO": "bar"}
                }
            }
        }"#;

        let config: McpConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.timeouts.connect_timeout, 15);
        assert_eq!(config.timeouts.execute_timeout, 90);
        assert_eq!(config.timeouts.read_timeout, 120); // default
        assert!(config.servers.contains_key("test"));

        let server = config.servers.get("test").unwrap();
        assert_eq!(server.command, Some("node".to_string()));
        assert_eq!(server.args, vec!["server.js"]);
        assert_eq!(server.env.get("FOO"), Some(&"bar".to_string()));
    }

    #[test]
    fn mcp_server_config_parses_custom_headers() {
        let json = r#"{
            "servers": {
                "hf": {
                    "url": "https://example.invalid/mcp",
                    "headers": {
                        "Authorization": "Bearer tok",
                        "X-Org": "anthropic"
                    }
                }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let hf = cfg.servers.get("hf").expect("server present");
        assert_eq!(
            hf.headers.get("Authorization"),
            Some(&"Bearer tok".to_string())
        );
        assert_eq!(hf.headers.get("X-Org"), Some(&"anthropic".to_string()));
    }

    #[test]
    fn mcp_server_config_omits_headers_when_empty() {
        // Empty headers map should not appear in the serialized output —
        // older mcp.json files written before v0.8.31 must round-trip
        // unchanged so a `mcp save` from a fresh install doesn't add
        // dead keys.
        let cfg = McpServerConfig {
            command: Some("node".into()),
            args: vec!["server.js".into()],
            env: HashMap::new(),
            url: None,
            transport: None,
            connect_timeout: None,
            execute_timeout: None,
            read_timeout: None,
            disabled: false,
            enabled: true,
            required: false,
            enabled_tools: Vec::new(),
            disabled_tools: Vec::new(),
            headers: HashMap::new(),
        };
        let serialized = serde_json::to_string(&cfg).unwrap();
        assert!(
            !serialized.contains("\"headers\""),
            "empty headers must be omitted: {serialized}"
        );
    }

    #[test]
    fn is_safe_custom_header_accepts_normal_auth_pairs() {
        assert!(is_safe_custom_header("Authorization", "Bearer tok"));
        assert!(is_safe_custom_header("X-Api-Key", "deadbeef"));
        assert!(is_safe_custom_header("x-org", "anthropic"));
    }

    #[test]
    fn is_safe_custom_header_rejects_empty_or_whitespace_key() {
        assert!(!is_safe_custom_header("", "value"));
        assert!(!is_safe_custom_header("   ", "value"));
    }

    #[test]
    fn is_safe_custom_header_rejects_response_splitting_values() {
        assert!(
            !is_safe_custom_header("X-Foo", "abc\r\nSet-Cookie: evil=1"),
            "CRLF in value must reject — response-splitting defense"
        );
        assert!(
            !is_safe_custom_header("X-Foo", "abc\nbar"),
            "bare LF in value must reject"
        );
        assert!(
            !is_safe_custom_header("X-Foo", "abc\rbar"),
            "bare CR in value must reject"
        );
    }

    #[test]
    fn is_safe_custom_header_rejects_protocol_framing_overrides() {
        // The MCP Streamable HTTP transport relies on its own
        // Accept / Content-Type values for protocol negotiation;
        // a stray user override would silently break tool discovery.
        assert!(!is_safe_custom_header("Accept", "text/plain"));
        assert!(!is_safe_custom_header("accept", "text/plain"));
        assert!(!is_safe_custom_header("Content-Type", "text/plain"));
        assert!(!is_safe_custom_header("CONTENT-TYPE", "x/y"));
    }

    #[test]
    fn default_mcp_http_get_accepts_json_and_event_stream() {
        let client = reqwest::Client::new();
        let request =
            with_default_mcp_http_headers(client.get("https://example.invalid/mcp"), false)
                .build()
                .unwrap();
        assert_eq!(
            request.headers().get(ACCEPT).and_then(|v| v.to_str().ok()),
            Some(MCP_HTTP_ACCEPT)
        );
        assert!(
            request.headers().get(CONTENT_TYPE).is_none(),
            "SSE GET requests should not advertise a JSON request body"
        );
    }

    #[test]
    fn default_mcp_http_post_accepts_json_and_event_stream() {
        let client = reqwest::Client::new();
        let request =
            with_default_mcp_http_headers(client.post("https://example.invalid/mcp"), true)
                .build()
                .unwrap();
        assert_eq!(
            request.headers().get(ACCEPT).and_then(|v| v.to_str().ok()),
            Some(MCP_HTTP_ACCEPT)
        );
        assert_eq!(
            request
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
    }

    #[test]
    fn streamable_http_transport_stores_headers() {
        let client = reqwest::Client::new();
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer xyz".to_string());
        let transport = StreamableHttpTransport::new(
            client,
            "https://example.invalid/mcp".to_string(),
            headers.clone(),
        );
        assert_eq!(transport.headers, headers);
    }

    #[test]
    fn test_mcp_config_parse_mcp_servers_alias_and_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        fs::write(
            &path,
            r#"{
              "mcpServers": {
                "disabled": {
                  "command": "node",
                  "args": ["server.js"],
                  "disabled": true
                }
              }
            }"#,
        )
        .unwrap();

        let cfg = load_config(&path).unwrap();
        assert!(cfg.servers.contains_key("disabled"));
        let snapshot = manager_snapshot_from_config(&path, true).unwrap();
        assert!(snapshot.restart_required);
        assert_eq!(snapshot.servers[0].name, "disabled");
        assert!(!snapshot.servers[0].enabled);
        assert_eq!(snapshot.servers[0].error.as_deref(), Some("disabled"));
    }

    #[test]
    fn test_mcp_config_rejects_traversal_path() {
        let err = load_config(Path::new("../mcp.json")).expect_err("traversal path should fail");
        assert!(
            format!("{err:#}").contains("cannot contain '..'"),
            "got: {err:#}"
        );
    }

    #[test]
    fn test_mcp_config_manager_actions_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");

        assert_eq!(init_config(&path, false).unwrap(), McpWriteStatus::Created);
        assert_eq!(
            init_config(&path, false).unwrap(),
            McpWriteStatus::SkippedExists
        );

        add_server_config(
            &path,
            "local".to_string(),
            Some("node".to_string()),
            None,
            vec!["server.js".to_string()],
            None,
        )
        .unwrap();
        set_server_enabled(&path, "local", false).unwrap();
        let disabled = manager_snapshot_from_config(&path, true).unwrap();
        let local = disabled
            .servers
            .iter()
            .find(|server| server.name == "local")
            .unwrap();
        assert!(!local.enabled);
        assert_eq!(local.transport, "stdio");

        remove_server_config(&path, "local").unwrap();
        let removed = manager_snapshot_from_config(&path, true).unwrap();
        assert!(removed.servers.iter().all(|server| server.name != "local"));
    }

    #[test]
    fn test_mcp_config_adds_explicit_url_transports() {
        for (transport, expected_label) in [
            ("sse", "sse"),
            ("sse-ide", "sse-ide"),
            ("ws", "ws"),
            ("ws-ide", "ws-ide"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("mcp.json");

            add_server_config(
                &path,
                "remote".to_string(),
                None,
                Some("https://example.com/v1/mcp".to_string()),
                Vec::new(),
                Some(transport.to_string()),
            )
            .unwrap();

            let cfg = load_config(&path).unwrap();
            assert_eq!(
                cfg.servers
                    .get("remote")
                    .and_then(|server| server.transport.as_deref()),
                Some(transport)
            );

            let snapshot = manager_snapshot_from_config(&path, false).unwrap();
            assert_eq!(snapshot.servers[0].transport, expected_label);
        }
    }

    #[test]
    fn test_mcp_config_rejects_unknown_transport() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");

        let err = add_server_config(
            &path,
            "bad".to_string(),
            None,
            Some("https://example.com/mcp".to_string()),
            Vec::new(),
            Some("ftp".to_string()),
        )
        .expect_err("unknown transport should fail");

        assert!(
            format!("{err:#}").contains("Unsupported MCP transport"),
            "got: {err:#}"
        );
    }

    #[test]
    fn test_server_effective_timeouts() {
        let global = McpTimeouts::default();

        let server_with_override = McpServerConfig {
            command: Some("test".to_string()),
            args: vec![],
            env: HashMap::new(),
            url: None,
            transport: None,
            connect_timeout: Some(20),
            execute_timeout: None,
            read_timeout: Some(180),
            disabled: false,
            enabled: true,
            required: false,
            enabled_tools: Vec::new(),
            disabled_tools: Vec::new(),
            headers: HashMap::new(),
        };

        assert_eq!(server_with_override.effective_connect_timeout(&global), 20);
        assert_eq!(server_with_override.effective_execute_timeout(&global), 60); // global default
        assert_eq!(server_with_override.effective_read_timeout(&global), 180);
    }

    #[test]
    fn test_mcp_pool_is_mcp_tool() {
        assert!(McpPool::is_mcp_tool("mcp__filesystem__read"));
        assert!(McpPool::is_mcp_tool("mcp__git__status"));
        assert!(McpPool::is_mcp_tool("mcp_filesystem_read"));
        assert!(McpPool::is_mcp_tool("mcp_git_status"));
        assert!(McpPool::is_mcp_tool("list_mcp_resources"));
        assert!(McpPool::is_mcp_tool("list_mcp_resource_templates"));
        assert!(McpPool::is_mcp_tool("read_mcp_resource"));
        assert!(!McpPool::is_mcp_tool("read_file"));
        assert!(!McpPool::is_mcp_tool("exec_shell"));
    }

    #[test]
    fn test_format_tool_result_text() {
        let result = serde_json::json!({
            "content": [
                {"type": "text", "text": "Hello, world!"}
            ]
        });
        assert_eq!(format_tool_result(&result), "Hello, world!");
    }

    #[test]
    fn test_format_tool_result_error() {
        let result = serde_json::json!({
            "isError": true,
            "content": [
                {"type": "text", "text": "Something went wrong"}
            ]
        });
        assert_eq!(format_tool_result(&result), "Error: Something went wrong");
    }

    #[test]
    fn test_format_tool_result_multiple_content() {
        let result = serde_json::json!({
            "content": [
                {"type": "text", "text": "Line 1"},
                {"type": "text", "text": "Line 2"},
                {"type": "image", "data": "base64..."}
            ]
        });
        let formatted = format_tool_result(&result);
        assert!(formatted.contains("Line 1"));
        assert!(formatted.contains("Line 2"));
        assert!(formatted.contains("[image content]"));
    }

    struct ScriptedValueTransport {
        sent: Arc<Mutex<Vec<serde_json::Value>>>,
        responses: VecDeque<Vec<u8>>,
    }

    #[async_trait::async_trait]
    impl McpTransport for ScriptedValueTransport {
        async fn send(&mut self, msg: Vec<u8>) -> Result<()> {
            self.sent
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&msg)?);
            Ok(())
        }

        async fn recv(&mut self) -> Result<Vec<u8>> {
            self.responses
                .pop_front()
                .context("scripted transport exhausted")
        }
    }

    struct HangingValueTransport {
        sent: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    #[async_trait::async_trait]
    impl McpTransport for HangingValueTransport {
        async fn send(&mut self, msg: Vec<u8>) -> Result<()> {
            self.sent
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&msg)?);
            Ok(())
        }

        async fn recv(&mut self) -> Result<Vec<u8>> {
            std::future::pending().await
        }
    }

    fn test_server_config() -> McpServerConfig {
        McpServerConfig {
            command: Some("mock".to_string()),
            args: Vec::new(),
            env: HashMap::new(),
            url: None,
            transport: None,
            connect_timeout: None,
            execute_timeout: None,
            read_timeout: None,
            disabled: false,
            enabled: true,
            required: false,
            enabled_tools: Vec::new(),
            disabled_tools: Vec::new(),
            headers: HashMap::new(),
        }
    }

    fn test_connection(transport: Box<dyn McpTransport>) -> McpConnection {
        McpConnection {
            name: "mock".to_string(),
            transport,
            tools: Vec::new(),
            resources: Vec::new(),
            resource_templates: Vec::new(),
            prompts: Vec::new(),
            request_id: AtomicU64::new(1),
            state: ConnectionState::Ready,
            config: test_server_config(),
            cancel_token: tokio_util::sync::CancellationToken::new(),
        }
    }

    fn json_frame(value: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&value).unwrap()
    }

    #[tokio::test]
    async fn call_method_skips_notifications_and_unmatched_responses() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let transport = ScriptedValueTransport {
            sent: Arc::clone(&sent),
            responses: VecDeque::from([
                json_frame(serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/progress",
                    "params": {"progress": 0.5}
                })),
                json_frame(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 99,
                    "result": {"ignored": true}
                })),
                json_frame(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {"ok": true}
                })),
            ]),
        };
        let mut conn = test_connection(Box::new(transport));

        let result = conn
            .call_method("tools/call", serde_json::json!({"name": "echo"}), 1)
            .await
            .unwrap();

        assert_eq!(result, serde_json::json!({"ok": true}));
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["jsonrpc"], "2.0");
        assert_eq!(sent[0]["id"], "1");
        assert_eq!(sent[0]["method"], "tools/call");
    }

    #[tokio::test]
    async fn call_method_times_out_while_waiting_for_response() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let mut conn = test_connection(Box::new(HangingValueTransport {
            sent: Arc::clone(&sent),
        }));

        let err = conn
            .call_method("tools/call", serde_json::json!({"name": "echo"}), 0)
            .await
            .expect_err("hung receive should time out");

        assert!(
            err.to_string()
                .contains("MCP method 'tools/call' on server 'mock' timed out after 0s"),
            "unexpected error: {err:#}"
        );
        assert_eq!(sent.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_mcp_pool_empty_config() {
        let pool = McpPool::new(McpConfig::default());
        assert!(pool.server_names().is_empty());
        assert!(pool.all_tools().is_empty());
    }

    /// #1267 part 2: a pool built without a source path has no file to watch,
    /// so `reload_if_config_changed` must short-circuit instead of trying
    /// to stat `/`.
    #[tokio::test]
    async fn reload_if_config_changed_is_noop_without_source_path() {
        let mut pool = McpPool::new(McpConfig::default());
        let reloaded = pool.reload_if_config_changed().await.unwrap();
        assert!(!reloaded, "no source path → no reload");
    }

    /// #1267 part 2: when the on-disk config is byte-unchanged, the lazy
    /// reload must not drop connections — every call to `get_or_connect`
    /// would otherwise pay a full reconnect cycle on networked filesystems
    /// where mtime granularity is coarse.
    #[tokio::test]
    async fn reload_if_config_changed_skips_when_content_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(&path, r#"{"servers":{}}"#).unwrap();
        let mut pool = McpPool::from_config_path(&path).unwrap();
        // Force the mtime to advance without changing content.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&path, r#"{"servers":{}}"#).unwrap();
        let reloaded = pool.reload_if_config_changed().await.unwrap();
        assert!(
            !reloaded,
            "content-unchanged config must not trigger a reload"
        );
    }

    /// #1267 part 2: when the on-disk config changes content, the next
    /// `reload_if_config_changed` call must swap in the new config and
    /// (would) drop all live connections. We can't stand up a real
    /// `McpConnection` in a unit test, so we observe the swap via the
    /// publicly-readable side: server names go from empty to non-empty.
    #[tokio::test]
    async fn reload_if_config_changed_swaps_config_on_content_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(&path, r#"{"servers":{}}"#).unwrap();
        let mut pool = McpPool::from_config_path(&path).unwrap();
        assert!(pool.server_names().is_empty());
        // Mutate the file so both the mtime and the hash change.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(
            &path,
            r#"{"servers":{"new":{"command":"echo","args":["hi"]}}}"#,
        )
        .unwrap();
        let reloaded = pool.reload_if_config_changed().await.unwrap();
        assert!(reloaded, "content-changed config must trigger reload");
        let names = pool.server_names();
        assert!(
            names.contains(&"new"),
            "expected new server in pool after reload, got {names:?}"
        );
    }

    /// #1267 part 2: hash-based comparison must be stable for byte-identical
    /// configs and distinct for differing configs.
    #[test]
    fn hash_mcp_config_is_stable_and_change_sensitive() {
        let a = McpConfig::default();
        let b = McpConfig::default();
        assert_eq!(hash_mcp_config(&a), hash_mcp_config(&b));
        let mut c = McpConfig::default();
        c.servers.insert(
            "x".into(),
            McpServerConfig {
                command: Some("/bin/echo".into()),
                args: vec!["hi".into()],
                env: Default::default(),
                url: None,
                transport: None,
                connect_timeout: None,
                execute_timeout: None,
                read_timeout: None,
                disabled: false,
                enabled: true,
                required: false,
                enabled_tools: Vec::new(),
                disabled_tools: Vec::new(),
                headers: HashMap::new(),
            },
        );
        assert_ne!(
            hash_mcp_config(&a),
            hash_mcp_config(&c),
            "hash must change when servers map changes"
        );
    }

    /// #1319: discovered tools must be sorted by name so the prompt prefix
    /// is stable across runs (cache-hit stability), even when the server
    /// returns them in arbitrary or paginated order.
    #[tokio::test]
    async fn discover_tools_sorts_by_name_for_cache_stability() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let transport = ScriptedValueTransport {
            sent: Arc::clone(&sent),
            responses: VecDeque::from([
                json_frame(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "tools": [
                            { "name": "zeta", "inputSchema": {} },
                            { "name": "alpha", "inputSchema": {} }
                        ],
                        "nextCursor": "page-2"
                    }
                })),
                json_frame(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": {
                        "tools": [
                            { "name": "mu", "inputSchema": {} },
                            { "name": "beta", "inputSchema": {} }
                        ]
                    }
                })),
            ]),
        };
        let mut conn = test_connection(Box::new(transport));
        conn.discover_tools().await.expect("discover");

        let names: Vec<&str> = conn.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["alpha", "beta", "mu", "zeta"],
            "tools must be sorted by name regardless of server order or pagination"
        );
    }

    #[tokio::test]
    async fn mcp_pool_call_tool_preserves_tool_names_with_dashes() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let transport = ScriptedValueTransport {
            sent: Arc::clone(&sent),
            responses: VecDeque::from([json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"ok": true}
            }))]),
        };
        let mut conn = test_connection(Box::new(transport));
        conn.name = "dephy".to_string();
        conn.tools = vec![McpTool {
            name: "company--search".to_string(),
            description: None,
            input_schema: serde_json::json!({}),
        }];

        let mut pool = McpPool::new(McpConfig {
            timeouts: McpTimeouts::default(),
            servers: HashMap::new(),
        });
        pool.config
            .servers
            .insert("dephy".to_string(), test_server_config());
        pool.connections.insert("dephy".to_string(), conn);

        let result = pool
            .call_tool(
                "mcp__dephy__company--search",
                serde_json::json!({"query": "dephy"}),
            )
            .await
            .unwrap();

        assert_eq!(result, serde_json::json!({"ok": true}));
        let sent = sent.lock().unwrap();
        assert_eq!(sent[0]["method"], "tools/call");
        assert_eq!(sent[0]["params"]["name"], "company--search");
        assert_eq!(
            sent[0]["params"]["arguments"],
            serde_json::json!({"query": "dephy"})
        );
    }

    #[tokio::test]
    async fn mcp_pool_all_tools_uses_canonical_names_and_truncates_descriptions() {
        let mut conn = test_connection(Box::new(ScriptedValueTransport {
            sent: Arc::new(Mutex::new(Vec::new())),
            responses: VecDeque::new(),
        }));
        conn.name = "files".to_string();
        conn.tools = vec![McpTool {
            name: "read".to_string(),
            description: Some("x".repeat(MAX_MCP_DESCRIPTION_LENGTH + 10)),
            input_schema: serde_json::json!({"type": "object"}),
        }];

        let mut pool = McpPool::new(McpConfig::default());
        pool.connections.insert("files".to_string(), conn);
        pool.config
            .servers
            .insert("files".to_string(), test_server_config());

        let tools = pool.all_tools();
        assert_eq!(tools[0].0, "mcp__files__read");
        let api_tools = pool.to_api_tools();
        let api_tool = api_tools
            .iter()
            .find(|tool| tool.name == "mcp__files__read")
            .expect("canonical MCP tool should be exposed");
        assert_eq!(
            api_tool.description.chars().count(),
            MAX_MCP_DESCRIPTION_LENGTH
        );
    }

    #[tokio::test]
    async fn ide_servers_only_expose_allowed_tools() {
        let mut config = test_server_config();
        config.url = Some("http://127.0.0.1:1/sse".to_string());
        config.transport = Some("sse-ide".to_string());
        let mut conn = test_connection(Box::new(ScriptedValueTransport {
            sent: Arc::new(Mutex::new(Vec::new())),
            responses: VecDeque::new(),
        }));
        conn.name = "ide".to_string();
        conn.config = config;
        conn.tools = vec![
            McpTool {
                name: "getDiagnostics".to_string(),
                description: None,
                input_schema: serde_json::json!({}),
            },
            McpTool {
                name: "getOpenEditorFiles".to_string(),
                description: None,
                input_schema: serde_json::json!({}),
            },
            McpTool {
                name: "executeCode".to_string(),
                description: None,
                input_schema: serde_json::json!({}),
            },
        ];

        let mut pool = McpPool::new(McpConfig::default());
        pool.connections.insert("ide".to_string(), conn);

        let names: Vec<String> = pool.all_tools().into_iter().map(|(name, _)| name).collect();
        assert_eq!(
            names,
            vec![
                "mcp__ide__getDiagnostics".to_string(),
                "mcp__ide__getOpenEditorFiles".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn call_tool_accepts_legacy_and_canonical_names() {
        for prefixed_name in ["mcp__dephy__search", "mcp_dephy_search"] {
            let sent = Arc::new(Mutex::new(Vec::new()));
            let transport = ScriptedValueTransport {
                sent: Arc::clone(&sent),
                responses: VecDeque::from([json_frame(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {"ok": true}
                }))]),
            };
            let mut conn = test_connection(Box::new(transport));
            conn.name = "dephy".to_string();
            conn.tools = vec![McpTool {
                name: "search".to_string(),
                description: None,
                input_schema: serde_json::json!({}),
            }];

            let mut pool = McpPool::new(McpConfig::default());
            pool.config
                .servers
                .insert("dephy".to_string(), test_server_config());
            pool.connections.insert("dephy".to_string(), conn);

            let result = pool
                .call_tool(prefixed_name, serde_json::json!({"query": "dephy"}))
                .await
                .unwrap();
            assert_eq!(result, serde_json::json!({"ok": true}));
            assert_eq!(sent.lock().unwrap()[0]["params"]["name"], "search");
        }
    }

    #[tokio::test]
    async fn auth_failures_are_cached_as_needs_auth() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = tempfile::tempdir().unwrap();
        let previous_home = std::env::var_os("HOME");
        let previous_userprofile = std::env::var_os("USERPROFILE");
        unsafe {
            std::env::set_var("HOME", home.path());
            std::env::set_var("USERPROFILE", home.path());
        }

        let mut config = test_server_config();
        config.url = Some("https://user:secret@example.invalid/mcp".to_string());
        config.transport = Some("ws".to_string());
        McpAuthCache::mark("remote", &config, "401 Unauthorized");
        let cache = McpAuthCache::load();
        let entry = cache
            .fresh_entry("remote")
            .expect("auth entry should be fresh");
        assert_eq!(
            entry.url.as_deref(),
            Some("https://***:***@example.invalid/mcp")
        );
        assert_eq!(entry.transport.as_deref(), Some("ws"));
        assert!(
            needs_auth_error("remote", entry)
                .to_string()
                .contains("needs-auth")
        );

        let mut pool = McpPool::new(McpConfig::default());
        pool.config.servers.insert("remote".to_string(), config);
        let err = match pool.get_or_connect("remote").await {
            Ok(_) => panic!("fresh auth cache should short-circuit connection"),
            Err(err) => err,
        };
        assert!(format!("{err:#}").contains("needs-auth"));

        unsafe {
            match previous_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match previous_userprofile {
                Some(value) => std::env::set_var("USERPROFILE", value),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
    }

    #[test]
    fn connect_tasks_respect_env_batch_size_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("MCP_SERVER_CONNECTION_BATCH_SIZE");
        unsafe {
            std::env::remove_var("MCP_SERVER_CONNECTION_BATCH_SIZE");
        }
        assert_eq!(
            connection_batch_size("MCP_SERVER_CONNECTION_BATCH_SIZE", 3),
            3
        );
        unsafe {
            std::env::set_var("MCP_SERVER_CONNECTION_BATCH_SIZE", "7");
        }
        assert_eq!(
            connection_batch_size("MCP_SERVER_CONNECTION_BATCH_SIZE", 3),
            7
        );
        unsafe {
            std::env::set_var("MCP_SERVER_CONNECTION_BATCH_SIZE", "0");
        }
        assert_eq!(
            connection_batch_size("MCP_SERVER_CONNECTION_BATCH_SIZE", 3),
            3
        );
        unsafe {
            match previous {
                Some(value) => std::env::set_var("MCP_SERVER_CONNECTION_BATCH_SIZE", value),
                None => std::env::remove_var("MCP_SERVER_CONNECTION_BATCH_SIZE"),
            }
        }
    }

    #[tokio::test]
    async fn websocket_transport_round_trips_json_rpc() {
        let _lock = lock_mcp_loopback_tests().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let message = websocket.next().await.unwrap().unwrap();
            assert_eq!(message.to_text().unwrap(), "{\"jsonrpc\":\"2.0\",\"id\":1}");
            websocket
                .send(Message::Text(
                    "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}".into(),
                ))
                .await
                .unwrap();
            let _ = websocket.close(None).await;
        });

        let mut transport = WebSocketTransport::connect(format!("ws://{addr}/mcp"), HashMap::new())
            .await
            .unwrap();
        transport
            .send(b"{\"jsonrpc\":\"2.0\",\"id\":1}".to_vec())
            .await
            .unwrap();
        let response = transport.recv().await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(value["result"]["ok"], true);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn json_rpc_session_error_is_marked_stale() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let transport = ScriptedValueTransport {
            sent: Arc::clone(&sent),
            responses: VecDeque::from([json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {
                    "code": -32001,
                    "message": "MCP session expired"
                }
            }))]),
        };
        let mut conn = test_connection(Box::new(transport));

        let err = conn
            .call_tool("search", serde_json::json!({"query": "dephy"}), 1)
            .await
            .expect_err("session error should fail");

        assert!(
            is_mcp_stale_session_error(&err),
            "JSON-RPC session error should be retryable, got: {err:#}"
        );
    }

    #[test]
    fn sse_transport_closed_is_retryable() {
        let err = anyhow::anyhow!("SSE transport closed");
        assert!(
            is_mcp_stale_session_error(&err),
            "closed SSE stream should force reconnect before retry"
        );
    }

    #[tokio::test]
    async fn discover_all_ignores_unsupported_optional_capabilities() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let transport = ScriptedValueTransport {
            sent: Arc::clone(&sent),
            responses: VecDeque::from([
                json_frame(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "tools": [
                            { "name": "search", "inputSchema": {} }
                        ]
                    }
                })),
                json_frame(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "error": {
                        "code": -32601,
                        "message": "resources not supported"
                    }
                })),
                json_frame(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "error": {
                        "code": -32601,
                        "message": "resource templates not supported"
                    }
                })),
                json_frame(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 4,
                    "error": {
                        "code": -32601,
                        "message": "prompts not supported"
                    }
                })),
            ]),
        };
        let mut conn = test_connection(Box::new(transport));

        conn.discover_all().await.expect("discover");

        assert_eq!(conn.tools.len(), 1);
        assert_eq!(conn.tools[0].name, "search");
        assert!(conn.resources.is_empty());
        assert!(conn.resource_templates.is_empty());
        assert!(conn.prompts.is_empty());
    }

    /// #1244: when an MCP stdio server fails to spawn, the underlying OS
    /// error (e.g. ENOENT for a missing binary) must reach the user via the
    /// snapshot.error string. Regression test for `err.to_string()` dropping
    /// the anyhow chain — without `{err:#}` the user sees only the opaque
    /// wrapper "MCP stdio spawn failed (...)" and has nothing to act on.
    #[tokio::test]
    async fn discover_snapshot_includes_underlying_spawn_error_in_chain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        fs::write(
            &path,
            r#"{
                "mcpServers": {
                    "broken": {
                        "command": "codesmith-tui-test-this-binary-does-not-exist-9f8e7d6c5b4a",
                        "args": []
                    }
                }
            }"#,
        )
        .unwrap();

        let snapshot = discover_manager_snapshot(&path, None, false).await.unwrap();
        let server = snapshot
            .servers
            .iter()
            .find(|s| s.name == "broken")
            .expect("broken server should appear in snapshot");
        let err = server
            .error
            .as_deref()
            .expect("broken server should have an error");
        let lowered = err.to_lowercase();
        assert!(
            lowered.contains("os error")
                || lowered.contains("not found")
                || lowered.contains("no such"),
            "expected underlying spawn error in chain, got: {err}"
        );
    }

    #[test]
    fn parse_sse_message_data_extracts_message_events() {
        let body = "event: message\r\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\r\n\r\n";
        let messages = parse_sse_message_data(body);
        assert_eq!(messages.len(), 1);
        let value: serde_json::Value = serde_json::from_slice(&messages[0]).unwrap();
        assert_eq!(value["id"], 1);
        assert!(value.get("result").is_some());
    }

    #[test]
    fn response_id_matches_string_and_numeric_echoes() {
        assert!(response_id_matches(Some(&serde_json::json!("1")), "1"));
        assert!(response_id_matches(Some(&serde_json::json!(1)), "1"));
        assert!(!response_id_matches(Some(&serde_json::json!("2")), "1"));
    }

    #[test]
    fn legacy_sse_transport_requires_explicit_config() {
        let mut server = test_server_config();
        server.url = Some("https://example.com/mcp/abc/sse".to_string());

        assert!(
            !is_legacy_sse_transport(&server),
            "/sse paths must not force legacy SSE without an explicit transport override"
        );

        server.transport = Some("sse".to_string());
        assert!(is_legacy_sse_transport(&server));

        server.transport = Some("SSE".to_string());
        assert!(is_legacy_sse_transport(&server));

        server.transport = Some("http".to_string());
        assert!(!is_legacy_sse_transport(&server));
    }

    #[test]
    fn find_sse_event_separator_accepts_lf_and_crlf() {
        assert_eq!(
            find_sse_event_separator("event: endpoint\n\n"),
            Some((15, 2))
        );
        assert_eq!(
            find_sse_event_separator("event: endpoint\r\n\r\n"),
            Some((15, 4))
        );
    }

    #[tokio::test]
    #[ignore = "flaky: requires a live TCP listener and is sensitive to port allocation races"]
    async fn mcp_connection_supports_streamable_http_event_stream_responses() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::{TcpListener, TcpStream};

        async fn read_http_request(socket: &mut TcpStream) -> String {
            let mut request = Vec::new();
            let mut buf = [0; 1024];
            let header_end = loop {
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0, "client closed before headers completed");
                request.extend_from_slice(&buf[..n]);
                if let Some(pos) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break pos + 4;
                }
            };

            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            let total_len = header_end + content_length;
            while request.len() < total_len {
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0, "client closed before body completed");
                request.extend_from_slice(&buf[..n]);
            }

            String::from_utf8(request).unwrap()
        }

        async fn write_json_sse(socket: &mut TcpStream, response: serde_json::Value) {
            let body = format!("event: message\ndata: {response}\n\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }

        let _lock = lock_mcp_loopback_tests().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let request = read_http_request(&mut socket).await;
                    assert!(request.starts_with("POST /mcp "));
                    assert!(
                        request.contains("Accept: application/json, text/event-stream")
                            || request.contains("accept: application/json, text/event-stream")
                    );
                    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
                    let value: serde_json::Value = serde_json::from_str(body).unwrap();
                    let method = value["method"].as_str().unwrap();

                    if method == "notifications/initialized" {
                        socket
                            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n")
                            .await
                            .unwrap();
                        return;
                    }

                    let id = value["id"].clone();
                    let result = match method {
                        "initialize" => serde_json::json!({
                            "protocolVersion": "2024-11-05",
                            "serverInfo": {"name": "mock-streamable", "version": "1.0.0"},
                            "capabilities": {"tools": {}, "resources": {}, "prompts": {}}
                        }),
                        "tools/list" => serde_json::json!({
                            "tools": [{
                                "name": "read_wiki_structure",
                                "description": "Read wiki structure",
                                "inputSchema": {"type": "object"}
                            }]
                        }),
                        "resources/list" => serde_json::json!({"resources": []}),
                        "resources/templates/list" => {
                            serde_json::json!({"resourceTemplates": []})
                        }
                        "prompts/list" => serde_json::json!({"prompts": []}),
                        other => panic!("unexpected method: {other}"),
                    };
                    write_json_sse(
                        &mut socket,
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": result
                        }),
                    )
                    .await;
                });
            }
        });

        let config = McpServerConfig {
            command: None,
            args: vec![],
            env: HashMap::new(),
            url: Some(format!("http://{addr}/mcp")),
            transport: None,
            connect_timeout: Some(2),
            execute_timeout: None,
            read_timeout: None,
            disabled: false,
            enabled: true,
            required: false,
            enabled_tools: Vec::new(),
            disabled_tools: Vec::new(),
            headers: HashMap::new(),
        };

        let conn = McpConnection::connect_with_policy(
            "deepwiki".to_string(),
            config,
            &McpTimeouts::default(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(conn.state(), ConnectionState::Ready);
        assert_eq!(conn.tools().len(), 1);
        assert_eq!(conn.tools()[0].name, "read_wiki_structure");

        server.abort();
    }

    #[test]
    fn mask_url_secrets_strips_userinfo() {
        let masked = mask_url_secrets("https://user:s3cret@host.example/api?foo=bar");
        assert!(masked.contains("***"), "expected masked userinfo: {masked}");
        assert!(!masked.contains("s3cret"), "secret leaked: {masked}");
        assert!(masked.contains("host.example"), "host preserved: {masked}");
    }

    #[test]
    fn mask_url_secrets_passes_through_clean_url() {
        assert_eq!(
            mask_url_secrets("https://api.example.com/mcp"),
            "https://api.example.com/mcp"
        );
    }

    #[test]
    fn redact_body_preview_masks_bearer_token() {
        let redacted = redact_body_preview("Authorization: Bearer abc.def.ghi end");
        assert!(redacted.contains("Bearer ***"), "redacted: {redacted}");
        assert!(!redacted.contains("abc.def.ghi"), "leaked: {redacted}");
    }

    #[test]
    fn redact_proxy_userinfo_strips_password() {
        // Corporate-style proxy URL with embedded creds — the
        // password must never reach the on-disk log file. URL strings
        // are assembled from placeholder constants via `format!` so the
        // literal source never contains a scheme-prefixed username +
        // password pair (colon-separated, `@`-terminated) that
        // GitGuardian's "Basic Auth String" detector would flag as a
        // committed credential.
        let (placeholder_user, placeholder_pass) = ("PLACEHOLDER_USER", "PLACEHOLDER_PASS");
        let with_creds = format!("http://{placeholder_user}:{placeholder_pass}@proxy.example/");
        let redacted = redact_proxy_userinfo(&with_creds);
        assert_eq!(redacted, "http://***@proxy.example/");
        assert!(!redacted.contains(placeholder_pass));
        assert!(!redacted.contains(placeholder_user));

        // User only (no password) — still redacted.
        let with_user_only = format!("https://{placeholder_user}@proxy.example:8080");
        let redacted = redact_proxy_userinfo(&with_user_only);
        assert_eq!(redacted, "https://***@proxy.example:8080");

        // No userinfo segment — pass through.
        let redacted = redact_proxy_userinfo("http://proxy.example:3128/");
        assert_eq!(redacted, "http://proxy.example:3128/");

        // `@` appears only in the path, not as userinfo separator —
        // must not be mistaken for credentials.
        let redacted = redact_proxy_userinfo("http://proxy.example/path@thing");
        assert_eq!(redacted, "http://proxy.example/path@thing");

        // Garbage input (no `://`) returned unchanged — the
        // surrounding warning log is the only caller and is already
        // handling the malformed-URL case.
        assert_eq!(redact_proxy_userinfo("not-a-url"), "not-a-url");
    }

    #[test]
    fn redact_body_preview_masks_api_key_param() {
        let redacted = redact_body_preview("error message api_key=sk-12345&other=val");
        assert!(redacted.contains("api_key=***"), "redacted: {redacted}");
        assert!(!redacted.contains("sk-12345"), "leaked: {redacted}");
        assert!(
            redacted.contains("other=val"),
            "non-secret preserved: {redacted}"
        );
    }

    /// #420: `StdioTransport::shutdown` reaps the child process by sending
    /// SIGTERM and giving it a brief grace period before drop fires SIGKILL.
    /// The test spawns `cat` (which exits immediately on stdin EOF / SIGTERM)
    /// and verifies the transport tears down cleanly. Unix-only because
    /// SIGTERM doesn't exist on Windows; on Windows the test would just
    /// duplicate the kill_on_drop path.
    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_transport_shutdown_terminates_child() {
        use tokio::process::Command as TokioCommand;
        let mut cmd = TokioCommand::new("cat");
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().expect("spawn cat");
        let pid = child.id().expect("child pid");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let mut transport = StdioTransport {
            child,
            stdin,
            reader: tokio::io::BufReader::new(stdout),
            stderr_tail: StderrTail::new(),
        };

        // shutdown() should send SIGTERM and complete within the grace window.
        let start = std::time::Instant::now();
        transport.shutdown().await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < STDIO_SHUTDOWN_GRACE + Duration::from_millis(500),
            "shutdown blocked beyond grace window: {elapsed:?}"
        );

        // The child should be reaped — kill(pid, 0) returning ESRCH means
        // the pid is gone. If it's still alive, kill(0) returns 0, which
        // means our shutdown didn't terminate it.
        // SAFETY: pid was just collected from a tokio Child we spawned.
        // libc::kill with signal 0 only checks pid existence and is
        // async-signal-safe.
        let still_alive = unsafe { libc::kill(pid as i32, 0) } == 0;
        assert!(
            !still_alive,
            "child {pid} survived StdioTransport::shutdown — SIGTERM not delivered"
        );
    }

    /// Mid-run MCP server crash: the v0.8.x spawn path used `Stdio::null` for
    /// stderr, so a server that died with a useful stderr message left the
    /// caller with only "Stdio transport closed". Now stderr is piped into a
    /// bounded ring buffer and surfaced when the read side fails.
    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_transport_recv_error_includes_stderr_tail() {
        use tokio::process::Command as TokioCommand;

        let mut cmd = TokioCommand::new("sh");
        cmd.arg("-c")
            .arg("echo 'mcp-server: failed to load plugin' 1>&2; exit 1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd.spawn().expect("spawn sh");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");

        let stderr_tail = StderrTail::new();
        {
            let tail = Arc::clone(&stderr_tail);
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tail.push(line).await;
                }
            });
        }

        let mut transport = StdioTransport {
            child,
            stdin,
            reader: tokio::io::BufReader::new(stdout),
            stderr_tail,
        };

        // Give the subprocess time to write its stderr line and exit.
        tokio::time::sleep(Duration::from_millis(300)).await;

        let err = transport
            .recv()
            .await
            .expect_err("expected transport closed error");
        let err_str = format!("{err}");
        assert!(
            err_str.contains("Stdio transport closed"),
            "missing closed marker in: {err_str}"
        );
        assert!(
            err_str.contains("mcp-server: failed to load plugin"),
            "stderr context missing from error: {err_str}"
        );
    }

    #[tokio::test]
    async fn sse_connect_waits_for_endpoint_before_first_send() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering as AtomicOrdering},
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let _lock = lock_mcp_loopback_tests().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let post_seen = Arc::new(AtomicBool::new(false));
        let server_post_seen = Arc::clone(&post_seen);
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let server_cancel = cancel_token.clone();

        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let post_seen = Arc::clone(&server_post_seen);
                let server_cancel = server_cancel.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0; 1024];
                    loop {
                        let n = socket.read(&mut buf).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        request.extend_from_slice(&buf[..n]);
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let request = String::from_utf8_lossy(&request);
                    if request.starts_with("GET /sse ") {
                        socket
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n",
                            )
                            .await
                            .unwrap();
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        socket
                            .write_all(b"event: endpoint\ndata: /messages\n\n")
                            .await
                            .unwrap();
                        server_cancel.cancelled().await;
                    } else if request.starts_with("POST /messages ") {
                        post_seen.store(true, AtomicOrdering::SeqCst);
                        socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                            .await
                            .unwrap();
                    }
                });
            }
        });

        let client = reqwest::Client::new();
        let url = format!("http://{addr}/sse");
        let mut transport = SseTransport::connect(
            client,
            url,
            HashMap::new(),
            cancel_token.clone(),
            Duration::from_secs(2),
        )
        .await
        .unwrap();

        transport
            .send(json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize"
            })))
            .await
            .unwrap();

        assert!(
            post_seen.load(AtomicOrdering::SeqCst),
            "first SSE send should POST to the discovered endpoint"
        );

        cancel_token.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn sse_connect_accepts_crlf_endpoint_events() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering as AtomicOrdering},
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let _lock = lock_mcp_loopback_tests().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let post_seen = Arc::new(AtomicBool::new(false));
        let server_post_seen = Arc::clone(&post_seen);
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let server_cancel = cancel_token.clone();

        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let post_seen = Arc::clone(&server_post_seen);
                let server_cancel = server_cancel.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0; 1024];
                    loop {
                        let n = socket.read(&mut buf).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        request.extend_from_slice(&buf[..n]);
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let request = String::from_utf8_lossy(&request);
                    if request.starts_with("GET /sse ") {
                        socket
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n",
                            )
                            .await
                            .unwrap();
                        socket
                            .write_all(b"event: endpoint\r\ndata: /messages\r\n\r\n")
                            .await
                            .unwrap();
                        server_cancel.cancelled().await;
                    } else if request.starts_with("POST /messages ") {
                        post_seen.store(true, AtomicOrdering::SeqCst);
                        socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                            .await
                            .unwrap();
                    }
                });
            }
        });

        let client = reqwest::Client::new();
        let url = format!("http://{addr}/sse");
        let mut transport = SseTransport::connect(
            client,
            url,
            HashMap::new(),
            cancel_token.clone(),
            Duration::from_secs(2),
        )
        .await
        .unwrap();

        transport
            .send(json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize"
            })))
            .await
            .unwrap();

        assert!(
            post_seen.load(AtomicOrdering::SeqCst),
            "first SSE send should POST to the CRLF-discovered endpoint"
        );

        cancel_token.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn sse_transport_applies_custom_headers_to_get_and_post() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering as AtomicOrdering},
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let _lock = lock_mcp_loopback_tests().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let get_header_seen = Arc::new(AtomicBool::new(false));
        let post_header_seen = Arc::new(AtomicBool::new(false));
        let server_get_header_seen = Arc::clone(&get_header_seen);
        let server_post_header_seen = Arc::clone(&post_header_seen);
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let server_cancel = cancel_token.clone();

        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let get_header_seen = Arc::clone(&server_get_header_seen);
                let post_header_seen = Arc::clone(&server_post_header_seen);
                let server_cancel = server_cancel.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0; 1024];
                    loop {
                        let n = socket.read(&mut buf).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        request.extend_from_slice(&buf[..n]);
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let request = String::from_utf8_lossy(&request);
                    let request_lower = request.to_lowercase();
                    if request.starts_with("GET /sse ") {
                        if request_lower.contains("x-custom-auth: my-test-token") {
                            get_header_seen.store(true, AtomicOrdering::SeqCst);
                        }
                        socket
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n",
                            )
                            .await
                            .unwrap();
                        socket
                            .write_all(b"event: endpoint\ndata: /messages\n\n")
                            .await
                            .unwrap();
                        server_cancel.cancelled().await;
                    } else if request.starts_with("POST /messages ") {
                        if request_lower.contains("x-custom-auth: my-test-token") {
                            post_header_seen.store(true, AtomicOrdering::SeqCst);
                        }
                        socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                            .await
                            .unwrap();
                    }
                });
            }
        });

        let client = reqwest::Client::new();
        let url = format!("http://{addr}/sse");
        let mut headers = HashMap::new();
        headers.insert("X-Custom-Auth".to_string(), "my-test-token".to_string());
        let mut transport = SseTransport::connect(
            client,
            url,
            headers,
            cancel_token.clone(),
            Duration::from_secs(2),
        )
        .await
        .unwrap();

        transport
            .send(json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize"
            })))
            .await
            .unwrap();

        assert!(
            get_header_seen.load(AtomicOrdering::SeqCst),
            "legacy SSE GET must include user-configured custom headers"
        );
        assert!(
            post_header_seen.load(AtomicOrdering::SeqCst),
            "legacy SSE POST must include user-configured custom headers"
        );

        cancel_token.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn sse_post_error_includes_response_body_excerpt() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let _lock = lock_mcp_loopback_tests().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let server_cancel = cancel_token.clone();

        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let server_cancel = server_cancel.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0; 1024];
                    loop {
                        let n = socket.read(&mut buf).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        request.extend_from_slice(&buf[..n]);
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let request = String::from_utf8_lossy(&request);
                    if request.starts_with("GET /sse ") {
                        socket
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n",
                            )
                            .await
                            .unwrap();
                        socket
                            .write_all(b"event: endpoint\ndata: /messages\n\n")
                            .await
                            .unwrap();
                        server_cancel.cancelled().await;
                    } else if request.starts_with("POST /messages ") {
                        socket
                            .write_all(
                                b"HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: 25\r\n\r\n{\"error\":\"missing query\"}",
                            )
                            .await
                            .unwrap();
                    }
                });
            }
        });

        let client = reqwest::Client::new();
        let url = format!("http://{addr}/sse");
        let mut transport = SseTransport::connect(
            client,
            url,
            HashMap::new(),
            cancel_token.clone(),
            Duration::from_secs(2),
        )
        .await
        .unwrap();

        let err = transport
            .send(json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize"
            })))
            .await
            .expect_err("POST rejection should be returned");
        let err = format!("{err:#}");
        assert!(
            err.contains("400 Bad Request") && err.contains("missing query"),
            "SSE POST error should include status and body, got: {err}"
        );

        cancel_token.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn streamable_http_stale_session_reconnects_and_retries_tool_call() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let _lock = lock_mcp_loopback_tests().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let get_count = Arc::new(AtomicUsize::new(0));
        let stale_seen = Arc::new(AtomicBool::new(false));
        let success_seen = Arc::new(AtomicBool::new(false));
        let server_get_count = Arc::clone(&get_count);
        let server_stale_seen = Arc::clone(&stale_seen);
        let server_success_seen = Arc::clone(&success_seen);

        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let get_count = Arc::clone(&server_get_count);
                let stale_seen = Arc::clone(&server_stale_seen);
                let success_seen = Arc::clone(&server_success_seen);
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0; 4096];
                    let header_end = loop {
                        let n = socket.read(&mut buf).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        request.extend_from_slice(&buf[..n]);
                        if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos + 4;
                        }
                    };
                    let headers = String::from_utf8_lossy(&request[..header_end]).to_string();
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    while request.len() < header_end + content_length {
                        let n = socket.read(&mut buf).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        request.extend_from_slice(&buf[..n]);
                    }
                    let body = &request[header_end..header_end + content_length];
                    let session_header = headers.lines().find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("mcp-session-id")
                            .then(|| value.trim().to_string())
                    });

                    if headers.starts_with("GET /mcp ") {
                        let count = get_count.fetch_add(1, AtomicOrdering::SeqCst);
                        let session = if count == 0 { "sess-old" } else { "sess-new" };
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nMcp-Session-Id: {session}\r\nContent-Length: 0\r\n\r\n"
                        );
                        socket.write_all(response.as_bytes()).await.unwrap();
                        return;
                    }

                    let request_json: serde_json::Value = serde_json::from_slice(body).unwrap();
                    let method = request_json
                        .get("method")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    let id = request_json
                        .get("id")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!("0"));

                    if method == "tools/call" && session_header.as_deref() == Some("sess-old") {
                        stale_seen.store(true, AtomicOrdering::SeqCst);
                        socket
                            .write_all(
                                b"HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: 27\r\n\r\n{\"error\":\"session expired\"}",
                            )
                            .await
                            .unwrap();
                        return;
                    }

                    let result = match method {
                        "initialize" => serde_json::json!({
                            "protocolVersion": "2024-11-05",
                            "capabilities": {}
                        }),
                        "tools/list" => serde_json::json!({
                            "tools": [
                                { "name": "search", "inputSchema": {} }
                            ]
                        }),
                        "resources/list" => serde_json::json!({ "resources": [] }),
                        "resources/templates/list" => {
                            serde_json::json!({ "resourceTemplates": [] })
                        }
                        "prompts/list" => serde_json::json!({ "prompts": [] }),
                        "tools/call" => {
                            assert_eq!(session_header.as_deref(), Some("sess-new"));
                            success_seen.store(true, AtomicOrdering::SeqCst);
                            serde_json::json!({ "content": [{ "type": "text", "text": "ok" }] })
                        }
                        _ => {
                            socket
                                .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n")
                                .await
                                .unwrap();
                            return;
                        }
                    };
                    let response_body = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": result
                    })
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        response_body.len(),
                        response_body
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                });
            }
        });

        let mut cfg = McpConfig::default();
        cfg.servers.insert(
            "dephy".to_string(),
            McpServerConfig {
                command: None,
                args: Vec::new(),
                env: HashMap::new(),
                url: Some(format!("http://{addr}/mcp")),
                transport: None,
                connect_timeout: Some(10),
                execute_timeout: Some(10),
                read_timeout: None,
                disabled: false,
                enabled: true,
                required: false,
                enabled_tools: Vec::new(),
                disabled_tools: Vec::new(),
                headers: HashMap::new(),
            },
        );
        let mut pool = McpPool::new(cfg);

        let result = pool
            .call_tool(
                "mcp__dephy__search",
                serde_json::json!({ "query": "dephy" }),
            )
            .await
            .unwrap();

        assert_eq!(
            result,
            serde_json::json!({ "content": [{ "type": "text", "text": "ok" }] })
        );
        assert!(stale_seen.load(AtomicOrdering::SeqCst));
        assert!(success_seen.load(AtomicOrdering::SeqCst));
        assert_eq!(get_count.load(AtomicOrdering::SeqCst), 2);

        server.abort();
    }

    #[tokio::test]
    async fn legacy_sse_session_expiry_is_marked_stale() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        use tokio::sync::mpsc;

        let _lock = lock_mcp_loopback_tests().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 4096];
            let header_end = loop {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    return;
                }
                request.extend_from_slice(&buf[..n]);
                if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            assert!(headers.starts_with("POST /messages "));
            socket
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: 27\r\n\r\n{\"error\":\"session expired\"}",
                )
                .await
                .unwrap();
        });

        let (_sender, receiver) = mpsc::unbounded_channel();
        let mut transport = SseTransport {
            client: reqwest::Client::new(),
            base_url: format!("http://{addr}/sse"),
            headers: HashMap::new(),
            endpoint_url: Some(format!("http://{addr}/messages")),
            receiver,
            pending_messages: VecDeque::new(),
        };

        let err = transport
            .send(br#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#.to_vec())
            .await
            .expect_err("expired SSE session should fail");

        assert!(
            is_mcp_stale_session_error(&err),
            "SSE session expiry should be retryable, got: {err:#}"
        );

        server.abort();
    }

    #[tokio::test]
    async fn legacy_sse_closed_stream_reconnects_and_retries_tool_call() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::{TcpListener, TcpStream};
        use tokio::sync::mpsc;

        async fn read_http_request(socket: &mut TcpStream) -> (String, serde_json::Value) {
            let mut request = Vec::new();
            let mut buf = [0; 4096];
            let header_end = loop {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    return (String::new(), serde_json::Value::Null);
                }
                request.extend_from_slice(&buf[..n]);
                if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]).to_string();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while request.len() < header_end + content_length {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    return (headers, serde_json::Value::Null);
                }
                request.extend_from_slice(&buf[..n]);
            }
            let body = &request[header_end..header_end + content_length];
            let json = if body.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::from_slice(body).unwrap()
            };
            (headers, json)
        }

        let _lock = lock_mcp_loopback_tests().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let active_sse = Arc::new(Mutex::new(None::<mpsc::UnboundedSender<Option<String>>>));
        let get_count = Arc::new(AtomicUsize::new(0));
        let tool_call_count = Arc::new(AtomicUsize::new(0));
        let success_seen = Arc::new(AtomicBool::new(false));
        let server_active_sse = Arc::clone(&active_sse);
        let server_get_count = Arc::clone(&get_count);
        let server_tool_call_count = Arc::clone(&tool_call_count);
        let server_success_seen = Arc::clone(&success_seen);

        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let active_sse = Arc::clone(&server_active_sse);
                let get_count = Arc::clone(&server_get_count);
                let tool_call_count = Arc::clone(&server_tool_call_count);
                let success_seen = Arc::clone(&server_success_seen);
                tokio::spawn(async move {
                    let (headers, request_json) = read_http_request(&mut socket).await;
                    if headers.starts_with("GET /sse ") {
                        get_count.fetch_add(1, AtomicOrdering::SeqCst);
                        let (tx, mut rx) = mpsc::unbounded_channel::<Option<String>>();
                        *active_sse.lock().unwrap() = Some(tx);
                        socket
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n",
                            )
                            .await
                            .unwrap();
                        socket
                            .write_all(b"event: endpoint\ndata: /messages\n\n")
                            .await
                            .unwrap();
                        while let Some(message) = rx.recv().await {
                            let Some(message) = message else {
                                return;
                            };
                            let event = format!("event: message\ndata: {message}\n\n");
                            socket.write_all(event.as_bytes()).await.unwrap();
                        }
                        return;
                    }

                    if !headers.starts_with("POST /messages ") {
                        return;
                    }

                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                        .await
                        .unwrap();

                    let method = request_json
                        .get("method")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    if method == "notifications/initialized" {
                        return;
                    }

                    let id = request_json
                        .get("id")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!("0"));

                    if method == "tools/call" {
                        let count = tool_call_count.fetch_add(1, AtomicOrdering::SeqCst);
                        if count == 0 {
                            if let Some(tx) = active_sse.lock().unwrap().take() {
                                let _ = tx.send(None);
                            }
                            return;
                        }
                    }

                    let result = match method {
                        "initialize" => serde_json::json!({
                            "protocolVersion": "2024-11-05",
                            "capabilities": {}
                        }),
                        "tools/list" => serde_json::json!({
                            "tools": [
                                { "name": "search", "inputSchema": {} }
                            ]
                        }),
                        "resources/list" => serde_json::json!({ "resources": [] }),
                        "resources/templates/list" => {
                            serde_json::json!({ "resourceTemplates": [] })
                        }
                        "prompts/list" => serde_json::json!({ "prompts": [] }),
                        "tools/call" => {
                            success_seen.store(true, AtomicOrdering::SeqCst);
                            serde_json::json!({ "content": [{ "type": "text", "text": "ok" }] })
                        }
                        other => panic!("unexpected method: {other}"),
                    };
                    let response = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": result
                    })
                    .to_string();
                    if let Some(tx) = active_sse.lock().unwrap().as_ref() {
                        let _ = tx.send(Some(response));
                    }
                });
            }
        });

        let mut cfg = McpConfig::default();
        cfg.servers.insert(
            "dephy".to_string(),
            McpServerConfig {
                command: None,
                args: Vec::new(),
                env: HashMap::new(),
                url: Some(format!("http://{addr}/sse")),
                transport: Some("sse".to_string()),
                connect_timeout: Some(10),
                execute_timeout: Some(10),
                read_timeout: None,
                disabled: false,
                enabled: true,
                required: false,
                enabled_tools: Vec::new(),
                disabled_tools: Vec::new(),
                headers: HashMap::new(),
            },
        );
        let mut pool = McpPool::new(cfg);

        let result = pool
            .call_tool(
                "mcp__dephy__search",
                serde_json::json!({ "query": "dephy" }),
            )
            .await
            .unwrap();

        assert_eq!(
            result,
            serde_json::json!({ "content": [{ "type": "text", "text": "ok" }] })
        );
        assert_eq!(tool_call_count.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(get_count.load(AtomicOrdering::SeqCst), 2);
        assert!(success_seen.load(AtomicOrdering::SeqCst));

        server.abort();
    }

    #[test]
    fn session_id_starts_none() {
        let transport = StreamableHttpTransport::new(
            reqwest::Client::new(),
            "https://example.invalid/mcp".to_string(),
            HashMap::new(),
        );
        assert!(transport.session_id.is_none());
    }

    /// Session ID captured from a POST response is replayed on the next POST.
    #[tokio::test]
    async fn session_id_captured_from_post_response_and_replayed() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let _lock = lock_mcp_loopback_tests().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(req.starts_with("POST "), "expected POST, got: {req}");

            // First POST: return a session ID so the transport captures it.
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: sess-abc-123\r\nContent-Length: 2\r\n\r\n{}",
                )
                .await
                .unwrap();
            socket.flush().await.unwrap();

            // Read the second POST — should contain the session ID.
            let mut buf2 = [0u8; 4096];
            let n2 = socket.read(&mut buf2).await.unwrap();
            let req2 = String::from_utf8_lossy(&buf2[..n2]);
            // reqwest lower-cases header names.
            let req2_lower = req2.to_lowercase();
            assert!(
                req2_lower.contains("mcp-session-id: sess-abc-123"),
                "second POST must replay captured session ID, got:\n{req2}"
            );

            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });

        let client = reqwest::Client::new();
        let url = format!("http://{addr}/mcp");
        let mut transport = StreamableHttpTransport::new(client, url, HashMap::new());

        // First send: server returns Mcp-Session-Id.
        transport
            .send(json_frame(serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "method": "initialize",
                "params": {}
            })))
            .await
            .unwrap();
        assert_eq!(
            transport.session_id.as_deref(),
            Some("sess-abc-123"),
            "session ID should be captured from response"
        );

        // Second send: should replay the session ID.
        transport
            .send(json_frame(serde_json::json!({
                "jsonrpc": "2.0", "id": 2,
                "method": "tools/list",
                "params": {}
            })))
            .await
            .unwrap();

        server.abort();
    }

    /// Custom headers configured in McpServerConfig are applied to the GET
    /// preflight so servers that require auth on session-establishment GET
    /// (e.g. Hindsight, #1629) can authenticate it.
    #[tokio::test]
    async fn custom_headers_applied_to_get_preflight() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let _lock = lock_mcp_loopback_tests().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // The test signals success by writing to this flag — the GET handler
        // sets it when it sees the expected header.
        let header_seen = Arc::new(AtomicBool::new(false));
        let header_seen_srv = Arc::clone(&header_seen);

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);

            // reqwest lower-cases header names.
            if req.starts_with("GET ")
                && req.to_lowercase().contains("x-custom-auth: my-test-token")
            {
                header_seen_srv.store(true, AtomicOrdering::SeqCst);
            }

            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });

        let client = reqwest::Client::new();
        let url = format!("http://{addr}/mcp");
        let mut headers = HashMap::new();
        headers.insert("X-Custom-Auth".to_string(), "my-test-token".to_string());

        let mut transport = HttpTransport::new(
            client,
            url,
            headers,
            tokio_util::sync::CancellationToken::new(),
            Duration::from_secs(10),
        );

        transport.try_establish_session().await.unwrap();

        server.abort();

        assert!(
            header_seen.load(AtomicOrdering::SeqCst),
            "GET preflight must include user-configured custom headers"
        );
    }
}
