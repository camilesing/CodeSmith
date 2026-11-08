//! codesmith backend child process + REST client.
//!
//! Known limitations: `--port 0` is not supported by `codesmith serve`, so a
//! free port is picked by binding :0 and releasing before spawn (a small
//! loopback race, retried once on bind failure of the child).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;
use tokio::process::{Child, Command};
use tokio::time::sleep;

pub struct CodesmithProcess {
    pub base_url: String,
    pub token: String,
    client: reqwest::Client,
    child: Option<Child>,
}

fn pick_free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .context("codesmith backend: bind :0 to find a free port")?;
    Ok(listener.local_addr()?.port())
}

fn resolve_binary(configured: Option<&PathBuf>) -> Result<PathBuf> {
    if let Some(path) = configured {
        if path.is_file() {
            return Ok(path.clone());
        }
        bail!(
            "desktop.codesmith_path points at a missing file: {} \
             (fix the path, or unset it to use `codesmith` from PATH)",
            path.display()
        );
    }
    let from_path = which::which("codesmith").context(
        "codesmith binary not found for the desktop backend: install it \
         (cargo install --path crates/cli) or set desktop.codesmith_path",
    )?;
    Ok(from_path)
}

impl CodesmithProcess {
    /// Spawn `codesmith serve --http` on a fresh loopback port with a fresh
    /// bearer token, and wait for `/health`.
    pub async fn spawn(codesmith_path: Option<&PathBuf>) -> Result<Self> {
        let binary = resolve_binary(codesmith_path)?;
        let mut last_err = None;
        for _ in 0..2 {
            let port = pick_free_port()?;
            let token = uuid::Uuid::new_v4().to_string();
            let child = Command::new(&binary)
                .arg("serve")
                .arg("--http")
                .arg("--port")
                .arg(port.to_string())
                .arg("--auth-token")
                .arg(&token)
                .stdin(Stdio::null())
                // Inherit: the backend's own logs (engine errors, bind info)
                // belong in the desktop app's stderr, not a black hole.
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .kill_on_drop(true)
                .spawn()
                .with_context(|| format!("spawn {} serve --http", binary.display()))?;
            // `kill_on_drop` ties the child to this handle; leak the handle
            // into the struct so the process lives until app shutdown.
            let base_url = format!("http://127.0.0.1:{port}");
            let this = Self {
                base_url,
                token,
                client: reqwest::Client::new(),
                child: Some(child),
            };
            match this.wait_healthy().await {
                Ok(()) => {
                    eprintln!(
                        "codesmith backend: {} serve --http on {}",
                        binary.display(),
                        this.base_url
                    );
                    return Ok(this);
                }
                Err(err) => {
                    last_err = Some(err);
                }
            }
        }
        bail!("codesmith backend failed to become healthy: {last_err:?}")
    }

    async fn wait_healthy(&self) -> Result<()> {
        for _ in 0..100 {
            if let Ok(resp) = self
                .client
                .get(format!("{}/health", self.base_url))
                .send()
                .await
            {
                if resp.status().is_success() {
                    return Ok(());
                }
            }
            sleep(Duration::from_millis(100)).await;
        }
        bail!("no /health response within 10s from {}", self.base_url)
    }

    fn auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        builder.bearer_auth(&self.token)
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self
            .auth(self.client.get(format!("{}{path}", self.base_url)))
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        if !resp.status().is_success() {
            bail!("GET {path}: {}", resp.status());
        }
        resp.json()
            .await
            .with_context(|| format!("GET {path} body"))
    }

    pub async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T> {
        let resp = self
            .auth(self.client.post(format!("{}{path}", self.base_url)))
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        if !resp.status().is_success() {
            bail!(
                "POST {path}: {} {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            );
        }
        resp.json()
            .await
            .with_context(|| format!("POST {path} body"))
    }

    /// POST whose failure the caller renders as words rather than an error
    /// state (a command result notice): the inner `Err` carries the
    /// runtime's own message from its `{"error":{"message"}}` body, so a
    /// 409 reads as the runtime's conflict sentence instead of
    /// `POST …: 409 {…}`. Transport-level trouble stays an outer `Err`.
    pub async fn post_with_error_message<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<std::result::Result<T, String>> {
        let resp = self
            .auth(self.client.post(format!("{}{path}", self.base_url)))
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let message = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|body| {
                    body.pointer("/error/message")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| format!("HTTP {status}"));
            return Ok(Err(message));
        }
        resp.json()
            .await
            .with_context(|| format!("POST {path} body"))
            .map(Ok)
    }

    /// POST a waterfall settlement (approval decision / question answers).
    /// HTTP 404 reads as `Ok(false)`: the runtime answers 404 when nothing is
    /// pending, and a late client answer after the runtime's own timeout or
    /// decision must stay an idempotent no-op, not an error.
    pub async fn post_settle(&self, path: &str, body: &serde_json::Value) -> Result<bool> {
        let resp = self
            .auth(self.client.post(format!("{}{path}", self.base_url)))
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        if !resp.status().is_success() {
            bail!(
                "POST {path}: {} {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            );
        }
        Ok(true)
    }

    pub async fn post_empty(&self, path: &str) -> Result<()> {
        let resp = self
            .auth(self.client.post(format!("{}{path}", self.base_url)))
            .json(&serde_json::json!({}))
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        if !resp.status().is_success() {
            bail!(
                "POST {path}: {} {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            );
        }
        Ok(())
    }

    pub async fn patch<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T> {
        let resp = self
            .auth(self.client.patch(format!("{}{path}", self.base_url)))
            .json(body)
            .send()
            .await
            .with_context(|| format!("PATCH {path}"))?;
        if !resp.status().is_success() {
            bail!(
                "PATCH {path}: {} {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            );
        }
        resp.json()
            .await
            .with_context(|| format!("PATCH {path} body"))
    }

    pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self
            .auth(self.client.delete(format!("{}{path}", self.base_url)))
            .send()
            .await
            .with_context(|| format!("DELETE {path}"))?;
        if !resp.status().is_success() {
            bail!(
                "DELETE {path}: {} {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            );
        }
        resp.json()
            .await
            .with_context(|| format!("DELETE {path} body"))
    }

    /// One request whose HTTP failure the caller classifies by status (the
    /// queue mutations map 404/409 onto client error codes). Transport
    /// trouble stays an outer `Err`; an HTTP failure is
    /// `Ok(Err((status, message)))` carrying the runtime's own
    /// `{"error":{"message"}}` text when it sent one.
    pub async fn request_with_status(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<std::result::Result<serde_json::Value, (reqwest::StatusCode, String)>> {
        let mut builder = self
            .client
            .request(method.clone(), format!("{}{path}", self.base_url));
        if let Some(body) = body {
            builder = builder.json(body);
        }
        let resp = self
            .auth(builder)
            .send()
            .await
            .with_context(|| format!("{method} {path}"))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let message = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|body| {
                    body.pointer("/error/message")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| format!("HTTP {status}"));
            return Ok(Err((status, message)));
        }
        resp.json()
            .await
            .with_context(|| format!("{method} {path} body"))
            .map(Ok)
    }

    /// A long-lived SSE body as raw text chunks; the caller parses `data:`
    /// lines (keep-alive comments carry no data and are skipped naturally).
    pub async fn get_sse_lines(
        &self,
        path: &str,
    ) -> Result<futures_util::stream::BoxStream<'static, Result<String>>> {
        use futures_util::StreamExt;
        let resp = self
            .auth(self.client.get(format!("{}{path}", self.base_url)))
            .send()
            .await
            .with_context(|| format!("GET {path} (sse)"))?;
        if !resp.status().is_success() {
            bail!("GET {path}: {}", resp.status());
        }
        Ok(resp
            .bytes_stream()
            .map(|chunk| {
                chunk
                    .map_err(|err| anyhow::anyhow!("sse body: {err}"))
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            })
            .boxed())
    }
}

impl Drop for CodesmithProcess {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }
}
