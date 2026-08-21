//! Large-output routing for tool results (issue #548).
//!
//! Any tool result whose estimated token count exceeds the configured threshold
//! is intercepted here before it reaches the parent context. A lightweight
//! V4-Flash synthesis sub-agent condenses the raw output; only the synthesis
//! is returned to the parent. The raw content is stored in the workshop
//! variable `last_tool_result` so the parent agent can call
//! `promote_to_context` later if it needs the full text.
//!
//! Per-tool thresholds can override the global default. Individual tool calls
//! may pass `raw=true` to bypass routing entirely.

#[cfg(test)]
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub use crate::config_types::{DEFAULT_LARGE_OUTPUT_THRESHOLD_TOKENS, WorkshopConfig};
use codesmith_tools::ToolResult;

// ── Constants ──────────────────────────────────────────────────────────────────

/// Workshop variable name where the raw tool output is stored.
pub const WORKSHOP_LAST_TOOL_RESULT_VAR: &str = "last_tool_result";

// ── Configuration ─────────────────────────────────────────────────────────────

/// A resolved handle to the configured utility model (`[utility_model]`):
/// the LLM client plus the model id to send with each assist request.
///
/// The engine resolves this once at build time. Same-provider configurations
/// reuse the main client handle with a per-request model override; a
/// cross-provider configuration carries a dedicated second client. Consumers
/// (workshop synthesis, auto-route classification, seams) treat it as an
/// optional optimisation — when absent they fall back to the main model.
#[derive(Clone)]
pub struct UtilityLlm {
    /// Client handle for the utility model's provider.
    pub client: crate::llm_client::LlmClientHandle,
    /// Model id filled into `MessageRequest.model` for assist calls.
    pub model: String,
}

impl std::fmt::Debug for UtilityLlm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The trait-object handle has no Debug; the model id is the part that
        // matters in engine/host debug dumps.
        f.debug_struct("UtilityLlm")
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

// ── Token estimation ──────────────────────────────────────────────────────────

/// Estimate the number of tokens in `text`.
///
/// Delegates to the process-wide [`crate::tokenizer::TokenCounter`] — the
/// historical `chars/3` heuristic by default, exact counts when a
/// tokenizer.json was loaded via `[context].tokenizer_path`. The heuristic
/// is deliberately conservative (under-counts tokens) so we route
/// aggressively rather than letting a 5K-token blob slip through.
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    crate::tokenizer::default_counter().count_text(text)
}

// ── Router ────────────────────────────────────────────────────────────────────

/// Decision returned by [`LargeOutputRouter::route`].
#[derive(Debug, Clone, PartialEq)]
pub enum RouteDecision {
    /// The output is small enough; pass it through unmodified.
    PassThrough,
    /// The output exceeded the threshold and was (or should be) synthesised.
    Synthesise {
        /// Estimated token count of the raw output.
        estimated_tokens: usize,
        /// The threshold that was breached.
        threshold: usize,
    },
}

/// Intercepts tool results and routes large ones through the workshop.
///
/// This type is intentionally `Clone` and `Default` so it can be embedded
/// cheaply in `ToolContext` without
/// requiring `Arc` wrappers.
#[derive(Debug, Clone, Default)]
pub struct LargeOutputRouter {
    config: WorkshopConfig,
}

impl LargeOutputRouter {
    /// Construct a router from the resolved workshop config.
    #[must_use]
    pub fn new(config: WorkshopConfig) -> Self {
        Self { config }
    }

    /// Decide whether `result` for `tool_name` should be synthesised.
    ///
    /// Pass `raw_bypass = true` when the tool call included `raw = true`.
    #[must_use]
    pub fn route(&self, tool_name: &str, result: &ToolResult, raw_bypass: bool) -> RouteDecision {
        if raw_bypass || !result.success {
            return RouteDecision::PassThrough;
        }
        let threshold = self.config.threshold_for(tool_name);
        let estimated_tokens = estimate_tokens(&result.content);
        if estimated_tokens > threshold {
            RouteDecision::Synthesise {
                estimated_tokens,
                threshold,
            }
        } else {
            RouteDecision::PassThrough
        }
    }

    /// Build the synthesis prompt sent to the utility-model workshop
    /// sub-agent.
    ///
    /// The prompt is intentionally terse — the utility model is a fast model
    /// and we just want a faithful summary, not deep reasoning.
    #[must_use]
    pub fn synthesis_prompt(tool_name: &str, raw_output: &str, estimated_tokens: usize) -> String {
        format!(
            "You are a synthesis assistant. The tool `{tool_name}` produced {estimated_tokens} tokens \
             of output that is too large to include directly in the parent context.\n\n\
             Summarise the output below into a concise, faithful synthesis of ≤ 800 words. \
             Preserve key facts, numbers, file paths, error messages, and any actionable \
             information. Do NOT add commentary or interpretation beyond what is in the source.\n\n\
             <raw_tool_output>\n{raw_output}\n</raw_tool_output>"
        )
    }

    /// Run the synthesis call for a routed large output through the utility
    /// LLM (#548 follow-up).
    ///
    /// Returns `None` on timeout (30s), transport error, or an empty
    /// synthesis so the caller keeps its truncation-preview fallback — the
    /// tool result is never blocked or failed by the synthesis step. On
    /// success the usage is reported to the cost side-channel (#526) so the
    /// assist shows up in the session cost.
    pub async fn synthesise_via_utility_llm(
        utility: &UtilityLlm,
        tool_name: &str,
        raw_output: &str,
        estimated_tokens: usize,
    ) -> Option<String> {
        use crate::models::{ContentBlock, Message, MessageRequest};

        let request = MessageRequest {
            model: utility.model.clone(),
            messages: vec![Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: Self::synthesis_prompt(tool_name, raw_output, estimated_tokens),
                    cache_control: None,
                }],
            }],
            max_tokens: 1_024,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            stream: Some(false),
            temperature: Some(0.1),
            top_p: None,
        };

        let response = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            utility.client.create_message(request),
        )
        .await
        .ok()?
        .ok()?;

        let mut text = String::new();
        for block in &response.content {
            if let ContentBlock::Text { text: chunk, .. } = block
                && !chunk.trim().is_empty()
            {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(chunk);
            }
        }
        if text.trim().is_empty() {
            return None;
        }
        crate::cost_status::report(&utility.model, &response.usage);
        Some(text)
    }

    /// Wrap a synthesis result with a workshop provenance header and a hint
    /// about the stored raw output.
    #[must_use]
    pub fn wrap_synthesis(
        tool_name: &str,
        synthesis: &str,
        estimated_tokens: usize,
        threshold: usize,
    ) -> String {
        format!(
            "[workshop-synthesis: tool={tool_name}, raw_tokens≈{estimated_tokens}, \
             threshold={threshold}, raw_stored_in={WORKSHOP_LAST_TOOL_RESULT_VAR}]\n\n{synthesis}"
        )
    }
}

// ── Workshop variable store ───────────────────────────────────────────────────

/// In-process store for workshop variables that persist across tool calls
/// within a session. The only variable exposed today is `last_tool_result`
/// which holds the most recent raw large-tool output for `promote_to_context`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkshopVariables {
    /// Raw content of the most recent large tool output that was routed
    /// through the workshop. Empty string when no routing has occurred.
    #[serde(default)]
    pub last_tool_result: String,

    /// Name of the tool that produced `last_tool_result`.
    #[serde(default)]
    pub last_tool_name: String,
}

impl WorkshopVariables {
    /// Store the raw output from a large-tool routing event.
    pub fn store_raw(&mut self, tool_name: &str, raw: &str) {
        self.last_tool_result = raw.to_string();
        self.last_tool_name = tool_name.to_string();
    }

    /// Retrieve and clear the stored raw output (consume semantics so the
    /// variable is not accidentally promoted twice).
    ///
    /// Called by the `promote_to_context` tool (not yet wired in this PR).
    #[must_use]
    #[allow(dead_code)] // consumed by promote_to_context tool in follow-up
    pub fn take_raw(&mut self) -> Option<(String, String)> {
        if self.last_tool_result.is_empty() {
            return None;
        }
        let content = std::mem::take(&mut self.last_tool_result);
        let name = std::mem::take(&mut self.last_tool_name);
        Some((name, content))
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_client::{LlmClient, StreamEventBox};
    use crate::models::{ContentBlock, MessageRequest, MessageResponse, Usage};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    fn make_result(content: &str) -> ToolResult {
        ToolResult::success(content.to_string())
    }

    #[test]
    fn pass_through_below_threshold() {
        let router = LargeOutputRouter::default();
        let small = "x".repeat(100);
        let result = make_result(&small);
        assert_eq!(
            router.route("read_file", &result, false),
            RouteDecision::PassThrough
        );
    }

    #[test]
    fn synthesise_above_threshold() {
        let router = LargeOutputRouter::default();
        // DEFAULT threshold = 4096 tokens; 3 chars/token → 4096*3 = 12288 chars
        let big = "a".repeat(13_000);
        let result = make_result(&big);
        assert!(matches!(
            router.route("read_file", &result, false),
            RouteDecision::Synthesise { .. }
        ));
    }

    #[test]
    fn raw_bypass_skips_routing() {
        let router = LargeOutputRouter::default();
        let big = "a".repeat(13_000);
        let result = make_result(&big);
        // raw=true → always pass through regardless of size
        assert_eq!(
            router.route("exec_shell", &result, true),
            RouteDecision::PassThrough
        );
    }

    #[test]
    fn error_results_always_pass_through() {
        let router = LargeOutputRouter::default();
        let big = "error: ".repeat(2_000);
        let result = ToolResult::error(big);
        assert_eq!(
            router.route("exec_shell", &result, false),
            RouteDecision::PassThrough
        );
    }

    #[test]
    fn per_tool_threshold_override() {
        let mut per_tool = HashMap::new();
        per_tool.insert("grep_files".to_string(), 100); // very low
        let config = WorkshopConfig {
            large_output_threshold_tokens: Some(4096),
            per_tool_thresholds: Some(per_tool),
        };
        let router = LargeOutputRouter::new(config);
        // 100 tokens * 3 = 300 chars → trigger with 400 chars
        let medium = "b".repeat(400);
        let result = make_result(&medium);
        assert!(matches!(
            router.route("grep_files", &result, false),
            RouteDecision::Synthesise { .. }
        ));
        // Other tools still use the global threshold
        assert_eq!(
            router.route("read_file", &result, false),
            RouteDecision::PassThrough
        );
    }

    #[test]
    fn estimate_tokens_conservative() {
        // 9 chars → ceil(9/3) = 3 tokens
        assert_eq!(estimate_tokens("123456789"), 3);
        // 10 chars → ceil(10/3) = 4 tokens
        assert_eq!(estimate_tokens("1234567890"), 4);
        // Empty string
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn workshop_variables_store_and_take() {
        let mut vars = WorkshopVariables::default();
        assert!(vars.take_raw().is_none());

        vars.store_raw("read_file", "raw content here");
        let taken = vars.take_raw().expect("should have content");
        assert_eq!(taken.0, "read_file");
        assert_eq!(taken.1, "raw content here");

        // Second take is empty — consume semantics
        assert!(vars.take_raw().is_none());
    }

    #[test]
    fn wrap_synthesis_includes_provenance_header() {
        let wrapped = LargeOutputRouter::wrap_synthesis("web_search", "key facts here", 5000, 4096);
        assert!(wrapped.contains("workshop-synthesis"));
        assert!(wrapped.contains("web_search"));
        assert!(wrapped.contains("5000"));
        assert!(wrapped.contains("key facts here"));
    }

    // ── synthesis via utility LLM (#548 follow-up) ────────────────────────────

    /// Scripted utility client: replies with a fixed outcome and captures the
    /// last request so tests can assert on the synthesis call shape.
    struct ScriptedUtilityClient {
        reply: Result<String, String>,
        last_request: Mutex<Option<MessageRequest>>,
    }

    impl ScriptedUtilityClient {
        fn with_text(text: &str) -> Arc<Self> {
            Arc::new(Self {
                reply: Ok(text.to_string()),
                last_request: Mutex::new(None),
            })
        }

        fn failing() -> Arc<Self> {
            Arc::new(Self {
                reply: Err("transport exploded".to_string()),
                last_request: Mutex::new(None),
            })
        }

        fn captured_model(self: &Arc<Self>) -> String {
            self.last_request
                .lock()
                .unwrap()
                .as_ref()
                .expect("no request captured")
                .model
                .clone()
        }
    }

    impl LlmClient for ScriptedUtilityClient {
        fn provider_name(&self) -> &'static str {
            "scripted"
        }

        fn model(&self) -> &str {
            "scripted-utility"
        }

        fn create_message(
            &self,
            request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<MessageResponse>> + Send + '_>> {
            *self.last_request.lock().unwrap() = Some(request);
            let reply = self.reply.clone();
            Box::pin(async move {
                let text = reply.map_err(anyhow::Error::msg)?;
                Ok(MessageResponse {
                    id: "synth-1".to_string(),
                    r#type: "message".to_string(),
                    role: "assistant".to_string(),
                    content: vec![ContentBlock::Text {
                        text,
                        cache_control: None,
                    }],
                    model: "scripted-utility".to_string(),
                    stop_reason: None,
                    stop_sequence: None,
                    container: None,
                    usage: Usage::default(),
                })
            })
        }

        fn create_message_stream(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<StreamEventBox>> + Send + '_>> {
            Box::pin(async { Err(anyhow::anyhow!("not used in tests")) })
        }
    }

    fn utility(client: Arc<ScriptedUtilityClient>) -> UtilityLlm {
        UtilityLlm {
            client,
            model: "deepseek-v4-flash".to_string(),
        }
    }

    fn big_output() -> String {
        "x".repeat(13_000)
    }

    #[tokio::test]
    async fn synthesis_returns_text_and_targets_utility_model() {
        let client = ScriptedUtilityClient::with_text("faithful summary of the tool output");
        let out = LargeOutputRouter::synthesise_via_utility_llm(
            &utility(client.clone()),
            "exec_shell",
            &big_output(),
            5_000,
        )
        .await
        .expect("synthesis should succeed");
        assert_eq!(out, "faithful summary of the tool output");
        assert_eq!(client.captured_model(), "deepseek-v4-flash");
    }

    #[tokio::test]
    async fn synthesis_falls_back_on_client_error() {
        let out = LargeOutputRouter::synthesise_via_utility_llm(
            &utility(ScriptedUtilityClient::failing()),
            "exec_shell",
            &big_output(),
            5_000,
        )
        .await;
        assert!(out.is_none(), "transport errors must fall back to preview");
    }
}
