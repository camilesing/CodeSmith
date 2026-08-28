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

/// List models from the provider's `/models` endpoint (OpenAI-shaped
/// `{data: [{id, ...}]}`).
///
/// rig's `CompletionModel` has no list-models primitive, so this is a direct
/// `GET {base_url}/models` mirroring the hand-written client's
/// `api_url(base, "models")` + `parse_models_response`. Only the `deepseek`
/// factory wires an HTTP client into [`RigLlmClient`](super::RigLlmClient), so
/// the shim is effectively DeepSeek-only; other providers pass `None` and
/// inherit the `LlmClient::list_models` default (empty). The trait contract is
/// `Vec<String>`, so only the `id` field is kept (the `(owner)` display the old
/// client surfaced is dropped — a noted regression).
///
/// `{base}/models` (no `/v1` injection) matches the FIM/translate shim's URL
/// convention and resolves correctly for both `https://api.deepseek.com` and
/// `…/v1` base URLs (DeepSeek accepts either).
pub(crate) async fn list_models(
    http: &reqwest::Client,
    base_url: &str,
    api_key: &str,
) -> Result<Vec<String>> {
    let base = base_url.trim_end_matches('/');
    let url = format!("{base}/models");
    let resp = http.get(url).bearer_auth(api_key).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("list models error {status}: {text}"));
    }
    let parsed: serde_json::Value = resp.json().await?;
    Ok(parse_models_ids(&parsed))
}

/// Pure parser for the OpenAI-shaped `/models` payload `{data: [{id, ...}]}`,
/// returning sorted + de-duplicated model ids. Extracted from
/// [`list_models`] for testability (the HTTP round-trip is trivial; the JSON
/// shape is the part that can drift).
fn parse_models_ids(payload: &serde_json::Value) -> Vec<String> {
    let mut ids: Vec<String> = payload
        .get("data")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item.get("id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids.dedup();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_models_ids_extracts_sorts_and_dedups() {
        let payload = serde_json::json!({
            "data": [
                { "id": "deepseek-chat", "owned_by": "deepseek" },
                { "id": "deepseek-reasoner", "owned_by": "deepseek" },
                { "id": "deepseek-chat", "owned_by": "deepseek" },
            ]
        });
        assert_eq!(
            parse_models_ids(&payload),
            vec!["deepseek-chat".to_string(), "deepseek-reasoner".to_string()]
        );
    }

    #[test]
    fn parse_models_ids_handles_missing_or_empty_data() {
        assert!(parse_models_ids(&serde_json::json!({})).is_empty());
        assert!(parse_models_ids(&serde_json::json!({ "data": [] })).is_empty());
        // A non-array `data` is treated as no models rather than panicking.
        assert!(parse_models_ids(&serde_json::json!({ "data": "oops" })).is_empty());
    }
}
