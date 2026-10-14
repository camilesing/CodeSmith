//! Alibaba OpenSandbox backend adapter.
//!
//! Sends shell commands to an OpenSandbox-compatible HTTP API for remote
//! execution.  The API endpoint is `POST {base_url}/v1/sandbox/run` with a
//! structured JSON body containing `cmd`, `env`, `cwd`, `timeout_ms`, optional
//! `stdin`, and `policy`, and expects a JSON response
//! `{"stdout": "...", "stderr": "...", "exit_code": 0}`.

use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

use super::backend::{SandboxBackend, SandboxExecRequest, SandboxOutput};

/// Request body sent to the OpenSandbox `/v1/sandbox/run` endpoint.
type SandboxRunRequest = SandboxExecRequest;

/// Response body from the OpenSandbox `/v1/sandbox/run` endpoint.
#[derive(Debug, Deserialize)]
struct SandboxRunResponse {
    /// Standard output from the command.
    stdout: String,
    /// Standard error from the command.
    stderr: String,
    /// Exit code (0 for success).
    exit_code: i32,
}

/// An OpenSandbox-compatible remote execution backend.
///
/// Constructed with a base URL (e.g. `"http://localhost:8080"`), an optional
/// API key sent as a `Bearer` token, and a timeout in seconds.
///
/// Manual `Debug` so the API key never prints in plaintext.
pub struct OpenSandboxBackend {
    base_url: reqwest::Url,
    api_key: Option<String>,
    timeout_secs: u64,
    client: reqwest::Client,
}

impl std::fmt::Debug for OpenSandboxBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenSandboxBackend")
            .field("base_url", &self.base_url.as_str())
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
}

impl OpenSandboxBackend {
    /// Create a new OpenSandbox backend.
    ///
    /// `base_url` should be the root of the OpenSandbox API (e.g.
    /// `"http://localhost:8080"`). `api_key` is optional and sent as
    /// `Authorization: Bearer <key>` when set. `timeout_secs` controls the
    /// HTTP request timeout.
    pub fn new(base_url: String, api_key: Option<String>, timeout_secs: u64) -> Result<Self> {
        // Parse once and fail fast: the URL is built by string
        // concatenation downstream, so a misconfigured value with an
        // embedded path/query or a non-HTTP scheme would silently target a
        // completely different endpoint — one this adapter sends the Bearer
        // key and shell commands to.
        let base = reqwest::Url::parse(base_url.trim_end_matches('/'))
            .with_context(|| format!("invalid OpenSandbox base URL: {base_url:?}"))?;
        if !matches!(base.scheme(), "http" | "https") {
            anyhow::bail!("invalid OpenSandbox base URL {base_url:?}: scheme must be http(s)");
        }
        if api_key.is_some() && base.scheme() != "https" {
            // Plaintext HTTP would expose the bearer token on the wire;
            // loopback is the one reasonable local-dev exception.
            if base.host_str() != Some("localhost")
                && !base.host_str().is_some_and(|h| h.starts_with("127."))
            {
                anyhow::bail!(
                    "refusing to send the OpenSandbox API key over plaintext HTTP to {base_url:?}; \
                     use https or a loopback host"
                );
            }
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .context("failed to construct HTTP client for OpenSandbox backend")?;

        Ok(Self {
            base_url: base,
            api_key,
            timeout_secs,
            client,
        })
    }

    /// Build the full URL for the sandbox run endpoint.
    fn run_url(&self) -> String {
        // Append the endpoint path onto the parsed base (string-append
        // semantics: `{base}/v1/sandbox/run`), dropping any query/fragment
        // the configured base carried so a misconfigured value cannot
        // smuggle parameters into the request URL.
        let mut url = self.base_url.clone();
        url.set_path(&format!(
            "{}/v1/sandbox/run",
            self.base_url.path().trim_end_matches('/')
        ));
        url.set_query(None);
        url.set_fragment(None);
        url.to_string()
    }
}

#[async_trait]
impl SandboxBackend for OpenSandboxBackend {
    async fn exec(&self, request_body: SandboxRunRequest) -> Result<SandboxOutput> {
        // Honor the per-command budget plus transport overhead — the
        // client-wide default used to abort long-running commands
        // client-side with a misleading "Failed to reach" error while the
        // remote command kept executing.
        let budget = std::time::Duration::from_millis(request_body.timeout_ms)
            .saturating_add(std::time::Duration::from_secs(5));
        let mut req = self
            .client
            .post(self.run_url())
            .timeout(budget)
            .json(&request_body);

        if let Some(ref api_key) = self.api_key {
            req = req.bearer_auth(api_key);
        }

        let response = req
            .send()
            .await
            .context("Failed to reach OpenSandbox endpoint")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("OpenSandbox returned HTTP {}: {}", status.as_u16(), body);
        }

        let parsed: SandboxRunResponse = response
            .json()
            .await
            .context("Failed to parse OpenSandbox response")?;

        Ok(SandboxOutput {
            stdout: parsed.stdout,
            stderr: parsed.stderr,
            exit_code: parsed.exit_code,
        })
    }
}
