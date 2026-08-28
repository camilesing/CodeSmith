//! OAuth device-flow support for MCP client connections (#2190).
//!
//! Decision record: RFC 2190 originally proposed splitting the MCP crate
//! and adding OAuth to a hypothetical `codesmith-mcp-client`. The split was
//! dropped — the real MCP client already lives in `agent-runtime` and is
//! reused by every frontend — so OAuth landed here, next to the client it
//! authenticates.
//!
//! Scope:
//! - Device Code Flow (RFC 8628): provider preset for GitHub plus fully
//!   custom endpoints.
//! - Token storage via [`codesmith_secrets::Secrets`] (system keyring when
//!   available, permissioned-file fallback).
//! - Bearer injection at connect time for URL transports.
//! - One refresh-token round trip on 401 in the HTTP transports.
//!
//! Out of scope: PKCE/redirect flows, client-credentials grants, and OAuth
//! for stdio servers (stdio servers take credentials via their `env`
//! table).

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use codesmith_secrets::Secrets;

/// GitHub OAuth device-flow endpoints (provider preset).
pub const GITHUB_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
pub const GITHUB_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

/// Default polling cadence when the device-code response omits `interval`.
const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;
/// Extra delay added when the token endpoint answers `slow_down`.
const SLOW_DOWN_STEP_SECS: u64 = 5;
/// Clock-skew margin when deciding whether a stored token is expired.
const EXPIRY_SKEW_SECS: i64 = 60;

/// OAuth configuration for one MCP server — the `oauth` table inside a
/// server entry in `~/.codesmith/mcp.json`.
///
/// `provider = "github"` fills the standard GitHub endpoints; any other
/// provider must set `device_code_url` and `token_url` explicitly.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct McpOAuthConfig {
    pub provider: String,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_code_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
}

impl McpOAuthConfig {
    pub fn device_code_endpoint(&self) -> Result<&str> {
        if let Some(url) = self
            .device_code_url
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            return Ok(url);
        }
        if let Some((device, _)) = known_provider_endpoints(&self.provider) {
            return Ok(device);
        }
        bail!(
            "oauth provider `{}` has no known device-code endpoint; set `device_code_url` explicitly",
            self.provider
        );
    }

    pub fn token_endpoint(&self) -> Result<&str> {
        if let Some(url) = self.token_url.as_deref().filter(|s| !s.trim().is_empty()) {
            return Ok(url);
        }
        if let Some((_, token)) = known_provider_endpoints(&self.provider) {
            return Ok(token);
        }
        bail!(
            "oauth provider `{}` has no known token endpoint; set `token_url` explicitly",
            self.provider
        );
    }
}

fn known_provider_endpoints(provider: &str) -> Option<(&'static str, &'static str)> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "github" => Some((GITHUB_DEVICE_CODE_URL, GITHUB_TOKEN_URL)),
        _ => None,
    }
}

/// Stored token material for one MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpOAuthToken {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix timestamp (seconds) when the access token expires, if the
    /// provider reported a lifetime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

/// Per-connection OAuth context threaded into the HTTP transports so a 401
/// can trigger one refresh-token round trip.
#[derive(Debug, Clone)]
pub struct OAuthState {
    /// MCP server name from `mcp.json`; keys the token store.
    pub server: String,
    pub config: McpOAuthConfig,
}

/// Secret-store key for a server's token. Server names are normalized so
/// renaming case in the config does not orphan the stored token.
#[must_use]
pub fn secret_key(server: &str) -> String {
    format!("mcp_oauth/{}", server.trim().to_ascii_lowercase())
}

/// Default secret store for MCP OAuth tokens (system keyring when
/// available, permissioned-file fallback).
#[must_use]
pub fn default_secrets() -> Secrets {
    Secrets::auto_detect()
}

pub fn load_token_with(secrets: &Secrets, server: &str) -> Result<Option<McpOAuthToken>> {
    match secrets.get(&secret_key(server)) {
        Ok(Some(raw)) => Ok(Some(
            serde_json::from_str(&raw).context("failed to parse stored MCP OAuth token")?,
        )),
        Ok(None) => Ok(None),
        Err(err) => Err(anyhow::anyhow!(
            "failed to read MCP OAuth token from secret store: {err}"
        )),
    }
}

pub fn store_token_with(secrets: &Secrets, server: &str, token: &McpOAuthToken) -> Result<()> {
    let raw = serde_json::to_string(token).context("failed to serialize MCP OAuth token")?;
    secrets
        .set(&secret_key(server), &raw)
        .map_err(|err| anyhow::anyhow!("failed to store MCP OAuth token: {err}"))
}

/// Whether a stored token's lifetime has elapsed. Tokens without a
/// reported expiry never count as expired (GitHub's default user tokens do
/// not expire).
#[must_use]
pub fn token_is_expired(token: &McpOAuthToken) -> bool {
    let Some(expires_at) = token.expires_at else {
        return false;
    };
    let now = unix_now();
    now + EXPIRY_SKEW_SECS >= expires_at
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(i64::MAX)
}

fn expires_at_from_now(expires_in: u64) -> i64 {
    unix_now().saturating_add(expires_in as i64)
}

/// Resolve the `Authorization` header value for a server at connect time:
/// load the stored token, proactively refresh when expired and a refresh
/// token exists, and return `Bearer <token>`. `Ok(None)` means no token is
/// stored (the caller should hint at `codesmith mcp auth`).
pub async fn resolve_authorization_value(
    client: &reqwest::Client,
    secrets: &Secrets,
    server: &str,
    config: &McpOAuthConfig,
) -> Result<Option<String>> {
    let Some(mut token) = load_token_with(secrets, server)? else {
        return Ok(None);
    };
    if token_is_expired(&token) && token.refresh_token.is_some() {
        let state = OAuthState {
            server: server.to_string(),
            config: config.clone(),
        };
        let current = format!("Bearer {}", token.access_token);
        if let Ok(Some(fresh)) = try_refresh_matching(client, secrets, &state, &current).await {
            token = fresh;
        }
    }
    Ok(Some(format!("Bearer {}", token.access_token)))
}

/// Exchange the stored refresh token for a fresh access token and persist
/// it. `Ok(None)` means there is nothing to refresh from: no stored token,
/// the compared header is not the one we injected (the user configured
/// their own), or no refresh token exists.
pub async fn try_refresh_matching(
    client: &reqwest::Client,
    secrets: &Secrets,
    state: &OAuthState,
    current_bearer: &str,
) -> Result<Option<McpOAuthToken>> {
    let Some(stored) = load_token_with(secrets, &state.server)? else {
        return Ok(None);
    };
    let current = bearer_token_value(current_bearer);
    if current != stored.access_token {
        // Not our injected header — never overwrite a user-configured one.
        return Ok(None);
    }
    let Some(refresh_token) = stored.refresh_token.clone() else {
        return Ok(None);
    };

    let token_url = state.config.token_endpoint()?;
    let body = post_form_json(
        client,
        token_url,
        &[
            ("client_id", state.config.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
        ],
    )
    .await?;
    match parse_token_response(&body) {
        DeviceTokenOutcome::Granted(mut token) => {
            if token.refresh_token.is_none() {
                // Some providers omit the refresh token on renewal; keep
                // the previous one rather than losing refreshability.
                token.refresh_token = Some(refresh_token);
            }
            store_token_with(secrets, &state.server, &token)?;
            Ok(Some(token))
        }
        DeviceTokenOutcome::AuthorizationPending | DeviceTokenOutcome::SlowDown => Ok(None),
        DeviceTokenOutcome::Denied { error, description } => {
            tracing::warn!(
                target: "mcp_oauth",
                server = %state.server,
                error = %error,
                description = description.as_deref().unwrap_or(""),
                "token refresh denied; re-run `codesmith mcp auth {}`",
                state.server
            );
            Ok(None)
        }
    }
}

fn bearer_token_value(header_value: &str) -> &str {
    header_value
        .trim()
        .strip_prefix("Bearer ")
        .unwrap_or(header_value.trim())
        .trim()
}

/// The device-code grant a provider handed out (RFC 8628 §3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

pub fn parse_device_code_response(body: &str) -> Result<DeviceCode> {
    #[derive(Deserialize)]
    struct Raw {
        device_code: String,
        user_code: String,
        #[serde(alias = "verification_url")]
        verification_uri: String,
        #[serde(default = "default_expires_in")]
        expires_in: u64,
        #[serde(default = "default_interval")]
        interval: u64,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        error_description: Option<String>,
    }
    fn default_expires_in() -> u64 {
        900
    }
    fn default_interval() -> u64 {
        DEFAULT_POLL_INTERVAL_SECS
    }

    let raw: Raw = serde_json::from_str(body)
        .with_context(|| format!("invalid device-code response: {}", excerpt(body)))?;
    if let Some(error) = raw.error {
        bail!(
            "device-code request rejected ({error}): {}",
            raw.error_description.unwrap_or_default()
        );
    }
    Ok(DeviceCode {
        device_code: raw.device_code,
        user_code: raw.user_code,
        verification_uri: raw.verification_uri,
        expires_in: raw.expires_in,
        interval: raw.interval,
    })
}

/// One poll outcome from the token endpoint during a device flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceTokenOutcome {
    Granted(McpOAuthToken),
    AuthorizationPending,
    SlowDown,
    Denied {
        error: String,
        description: Option<String>,
    },
}

pub fn parse_token_response(body: &str) -> DeviceTokenOutcome {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        access_token: Option<String>,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        expires_in: Option<u64>,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        error_description: Option<String>,
    }

    let raw: Raw = match serde_json::from_str(body) {
        Ok(raw) => raw,
        Err(_) => {
            return DeviceTokenOutcome::Denied {
                error: "invalid_response".to_string(),
                description: Some(excerpt(body).to_string()),
            };
        }
    };
    if let Some(error) = raw.error {
        return match error.as_str() {
            "authorization_pending" => DeviceTokenOutcome::AuthorizationPending,
            "slow_down" => DeviceTokenOutcome::SlowDown,
            _ => DeviceTokenOutcome::Denied {
                error,
                description: raw.error_description,
            },
        };
    }
    match raw.access_token.filter(|token| !token.trim().is_empty()) {
        Some(access_token) => DeviceTokenOutcome::Granted(McpOAuthToken {
            access_token,
            refresh_token: raw.refresh_token.filter(|token| !token.trim().is_empty()),
            expires_at: raw.expires_in.map(expires_at_from_now),
        }),
        None => DeviceTokenOutcome::Denied {
            error: "missing_access_token".to_string(),
            description: None,
        },
    }
}

fn excerpt(body: &str) -> &str {
    let end = body
        .char_indices()
        .map(|(i, _)| i)
        .nth(200)
        .unwrap_or(body.len());
    &body[..end]
}

async fn post_form_json(
    client: &reqwest::Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<String> {
    let response = client
        .post(url)
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(30))
        .form(form)
        .send()
        .await
        .with_context(|| format!("OAuth request to {url} failed"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("OAuth endpoint {url} returned {status}: {}", excerpt(&body));
    }
    Ok(body)
}

/// Run the interactive device flow for a server and persist the resulting
/// token. Prints the verification URI / user code to stdout and blocks
/// until the user authorizes, the code expires, or the flow is denied.
pub async fn run_device_flow(
    client: &reqwest::Client,
    secrets: &Secrets,
    server: &str,
    config: &McpOAuthConfig,
) -> Result<McpOAuthToken> {
    let device_url = config.device_code_endpoint()?;
    let token_url = config.token_endpoint()?;
    let scope = config.scopes.join(" ");

    let body = post_form_json(
        client,
        device_url,
        &[
            ("client_id", config.client_id.as_str()),
            ("scope", scope.as_str()),
        ],
    )
    .await?;
    let code = parse_device_code_response(&body)?;

    println!();
    println!(
        "Authorization required for MCP server `{server}` ({}).",
        config.provider
    );
    println!("  1. Open:  {}", code.verification_uri);
    println!(
        "  2. Code:  {}   (expires in {}s)",
        code.user_code, code.expires_in
    );
    println!("Waiting for authorization…");

    let mut interval = Duration::from_secs(code.interval.max(1));
    let deadline = Instant::now() + Duration::from_secs(code.expires_in.max(1));
    loop {
        tokio::time::sleep(interval).await;
        if Instant::now() >= deadline {
            bail!("device code expired before authorization completed; re-run to retry");
        }
        let body = post_form_json(
            client,
            token_url,
            &[
                ("client_id", config.client_id.as_str()),
                ("device_code", code.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ],
        )
        .await?;
        match parse_token_response(&body) {
            DeviceTokenOutcome::Granted(token) => {
                store_token_with(secrets, server, &token)?;
                println!("Authorization stored for MCP server `{server}`.");
                return Ok(token);
            }
            DeviceTokenOutcome::AuthorizationPending => continue,
            DeviceTokenOutcome::SlowDown => {
                interval += Duration::from_secs(SLOW_DOWN_STEP_SECS);
                continue;
            }
            DeviceTokenOutcome::Denied { error, description } => {
                bail!(
                    "authorization denied ({error}): {}",
                    description.unwrap_or_default()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_secrets::InMemoryKeyringStore;
    use std::sync::Arc;

    fn memory_secrets() -> Secrets {
        Secrets::new(Arc::new(InMemoryKeyringStore::new()))
    }

    #[test]
    fn github_preset_fills_endpoints() {
        let config = McpOAuthConfig {
            provider: "GitHub".to_string(),
            client_id: "Iv1.test".to_string(),
            scopes: vec!["repo".to_string()],
            device_code_url: None,
            token_url: None,
        };
        assert_eq!(
            config.device_code_endpoint().unwrap(),
            GITHUB_DEVICE_CODE_URL
        );
        assert_eq!(config.token_endpoint().unwrap(), GITHUB_TOKEN_URL);
    }

    #[test]
    fn custom_provider_requires_explicit_endpoints() {
        let config = McpOAuthConfig {
            provider: "acme".to_string(),
            client_id: "cid".to_string(),
            scopes: vec![],
            device_code_url: None,
            token_url: None,
        };
        assert!(config.device_code_endpoint().is_err());
        assert!(config.token_endpoint().is_err());
    }

    #[test]
    fn oauth_config_parses_from_mcp_json_shape() {
        let raw = serde_json::json!({
            "provider": "github",
            "client_id": "Iv1.test",
            "scopes": ["repo", "read:org"]
        });
        let config: McpOAuthConfig = serde_json::from_value(raw).unwrap();
        assert_eq!(config.scopes, vec!["repo", "read:org"]);
        assert!(config.token_url.is_none());
    }

    #[test]
    fn server_entry_with_oauth_round_trips() {
        use crate::mcp::McpServerConfig;

        // The `oauth` table sits inside a server entry; make sure the
        // McpServerConfig field deserializes and skips serialization when
        // absent (older mcp.json files must round-trip unchanged).
        let without: McpServerConfig =
            serde_json::from_value(serde_json::json!({ "url": "https://example.invalid/mcp" }))
                .unwrap();
        assert!(without.oauth.is_none());
        assert!(!serde_json::to_string(&without).unwrap().contains("oauth"));

        let with: McpServerConfig = serde_json::from_value(serde_json::json!({
            "url": "https://example.invalid/mcp",
            "oauth": { "provider": "github", "client_id": "Iv1.test" }
        }))
        .unwrap();
        let oauth = with.oauth.clone().expect("oauth parsed");
        assert_eq!(oauth.provider, "github");
        assert_eq!(oauth.client_id, "Iv1.test");
        assert!(serde_json::to_string(&with).unwrap().contains("\"oauth\""));
    }

    #[test]
    fn token_store_round_trip() {
        let secrets = memory_secrets();
        let token = McpOAuthToken {
            access_token: "at".to_string(),
            refresh_token: Some("rt".to_string()),
            expires_at: Some(4_102_444_800),
        };
        assert!(load_token_with(&secrets, "github").unwrap().is_none());
        store_token_with(&secrets, "GitHub", &token).unwrap();
        // Key normalization: stored as `GitHub`, loaded as `github`.
        let loaded = load_token_with(&secrets, "github").unwrap().unwrap();
        assert_eq!(loaded, token);
    }

    #[test]
    fn parse_device_code_response_fields() {
        let body = r#"{
            "device_code": "dc",
            "user_code": "ABCD-1234",
            "verification_uri": "https://github.com/login/device",
            "expires_in": 900,
            "interval": 5
        }"#;
        let code = parse_device_code_response(body).unwrap();
        assert_eq!(code.user_code, "ABCD-1234");
        assert_eq!(code.interval, 5);

        let legacy = r#"{"device_code":"dc","user_code":"UC","verification_url":"https://x"}"#;
        assert_eq!(
            parse_device_code_response(legacy).unwrap().verification_uri,
            "https://x"
        );

        let err = r#"{"error":"invalid_client"}"#;
        assert!(parse_device_code_response(err).is_err());
    }

    #[test]
    fn parse_token_response_outcomes() {
        let granted = parse_token_response(r#"{"access_token":"at","expires_in":3600}"#);
        let DeviceTokenOutcome::Granted(token) = granted else {
            panic!("expected grant");
        };
        assert_eq!(token.access_token, "at");
        assert!(token.refresh_token.is_none());
        assert!(token.expires_at.is_some());

        assert_eq!(
            parse_token_response(r#"{"error":"authorization_pending"}"#),
            DeviceTokenOutcome::AuthorizationPending
        );
        assert_eq!(
            parse_token_response(r#"{"error":"slow_down"}"#),
            DeviceTokenOutcome::SlowDown
        );
        assert!(matches!(
            parse_token_response(r#"{"error":"access_denied"}"#),
            DeviceTokenOutcome::Denied { .. }
        ));
        // Empty access tokens must not be treated as granted.
        assert!(matches!(
            parse_token_response(r#"{"access_token":""}"#),
            DeviceTokenOutcome::Denied { .. }
        ));
        assert!(matches!(
            parse_token_response("not json"),
            DeviceTokenOutcome::Denied { .. }
        ));
    }

    #[test]
    fn expiry_checks_respect_skew_and_missing_lifetime() {
        let now = unix_now();
        let no_expiry = McpOAuthToken {
            access_token: "at".into(),
            refresh_token: None,
            expires_at: None,
        };
        assert!(!token_is_expired(&no_expiry));

        let fresh = McpOAuthToken {
            expires_at: Some(now + 3600),
            ..no_expiry.clone()
        };
        assert!(!token_is_expired(&fresh));

        let within_skew = McpOAuthToken {
            expires_at: Some(now + 30),
            ..no_expiry
        };
        assert!(token_is_expired(&within_skew));
    }
}
