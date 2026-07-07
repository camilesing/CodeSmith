//! Thin direct-HTTP shim for DeepSeek endpoints rig has no primitive for.
//!
//! rig's `CompletionModel` covers chat completions and streaming. Two
//! DeepSeek-specific operations the engine still calls through `LlmClient` are
//! not chat completions:
//!
//! - **FIM** (`/beta/completions`): fill-in-the-middle code completion, used by
//!   the inline-edit path. Takes a `prompt` + `suffix`, returns raw text.
//! - **translate** (`/chat/completions`): a one-shot system-prompted translation
//!   the engine uses for UI localization. It *is* a chat completion, but the
//!   engine wants a plain string back rather than a `MessageResponse`, so we
//!   keep a dedicated shim instead of routing through the rig adapter.
//!
//! Both hit the workspace `reqwest 0.13.x` client directly (the same one rig
//! uses internally) so there is exactly one HTTP stack in the build. The shim
//! is wired into `RigLlmClient` only when the factory supplies an HTTP client
//! (DeepSeek); other providers pass `None` and inherit the `LlmClient` default
//! "not supported" behaviour.

use anyhow::{Result, anyhow};

/// DeepSeek default base URL. Only the `deepseek` factory reaches the FIM/translate
/// shim, so the constant (and the [`resolve_base_url`] fallback that uses it) is
/// compiled only under that feature — keeping non-deepseek Lego builds clean.
#[cfg(feature = "deepseek")]
const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";

/// Fill-in-the-middle completion against DeepSeek's `/beta/completions`.
pub(crate) async fn fim_completion(
    http: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: String,
    prompt: String,
    suffix: String,
    max_tokens: u32,
) -> Result<String> {
    let base = base_url.trim_end_matches('/');
    let url = format!("{base}/beta/completions");
    let body = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "suffix": suffix,
        "max_tokens": max_tokens,
        "stream": false,
    });
    let resp = http
        .post(url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("DeepSeek FIM error {status}: {text}"));
    }
    let parsed: serde_json::Value = resp.json().await?;
    parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("text"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("DeepSeek FIM returned no choices.text"))
}

/// One-shot translation against DeepSeek's `/chat/completions`. Returns the
/// assistant message text.
pub(crate) async fn translate(
    http: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    text: String,
    model: String,
    target_language: String,
) -> Result<String> {
    let base = base_url.trim_end_matches('/');
    let url = format!("{base}/chat/completions");
    let body = serde_json::json!({
        "model": model,
        "stream": false,
        "messages": [
            { "role": "system", "content": format!("Translate the user's text into {target_language}. Reply with only the translation.") },
            { "role": "user", "content": text },
        ],
    });
    let resp = http
        .post(url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("DeepSeek translate error {status}: {text}"));
    }
    let parsed: serde_json::Value = resp.json().await?;
    parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("DeepSeek translate returned no choices.message.content"))
}

/// Resolve a possibly-empty configured base URL to the DeepSeek default.
#[cfg(feature = "deepseek")]
pub(crate) fn resolve_base_url(configured: &str) -> &str {
    if configured.is_empty() {
        DEFAULT_DEEPSEEK_BASE_URL
    } else {
        configured
    }
}
