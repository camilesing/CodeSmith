//! Alibaba OpenSandbox backend adapter.
//!
//! Sends shell commands to an OpenSandbox-compatible HTTP API for remote
//! execution.  The API endpoint is `POST {base_url}/v1/sandbox/run` with a
//! structured JSON body containing `cmd`, `env`, `cwd`, `timeout_ms`, optional
//! `stdin`, and `policy`, and expects a JSON response
//! `{"stdout": "...", "stderr": "...", "exit_code": 0}`.

use std::time::Duration;
use futures_util::StreamExt;

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
            if !codesmith_agent_runtime::network_policy::is_loopback_host(
                base.host_str().unwrap_or_default(),
            ) {
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
            let body = response
                .text()
                .await
                .context("failed to read OpenSandbox error body")?;
            let body: String = body.chars().take(1024).collect();
            anyhow::bail!("OpenSandbox returned HTTP {}: {}", status.as_u16(), body);
        }

        // The endpoint is trusted to run commands, not to be well-behaved
        // about response size: bound the accepted body so a misbehaving or
        // hostile endpoint cannot force unbounded memory use in the caller.
        // Enforced while streaming the DECODED body: `content_length()` is
        // absent for chunked responses and describes the (possibly
        // gzip-compressed) wire bytes, so a header check let both chunked
        // bodies and decompression bombs through unbounded.
        const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
        let mut body: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("failed to read OpenSandbox response body")?;
            anyhow::ensure!(
                body.len() + chunk.len() <= MAX_RESPONSE_BYTES,
                "OpenSandbox response too large (limit {MAX_RESPONSE_BYTES} bytes)"
            );
            body.extend_from_slice(&chunk);
        }

        let parsed: SandboxRunResponse = serde_json::from_slice(&body)
            .context("Failed to parse OpenSandbox response")?;

        Ok(SandboxOutput {
            stdout: parsed.stdout,
            stderr: parsed.stderr,
            exit_code: parsed.exit_code,
        })
    }
}
