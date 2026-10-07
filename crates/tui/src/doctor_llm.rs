//! Doctor LLM fallback layer — P3-8 ("持续进化闭环") step 2.
//!
//! The doctor's deterministic checks cover the high-frequency problems:
//! missing keys, unreachable endpoints, absent binaries. What they cannot
//! do is reason over the *combination* of findings — the long tail where
//! each individual hint looks reasonable but the real root cause sits in
//! how two findings interact (a proxy that eats the API host, a keyring
//! key shadowed by a stale env var, an alias model on a custom endpoint).
//!
//! This module is that fallback: after the deterministic checks complete,
//! the collected warnings/errors are serialized and handed to one advisory
//! LLM call (the `[utility_model]` when configured — the designated cheap
//! brain — falling back to the main client) which proposes root causes
//! and one concrete next action per finding.
//!
//! Design contract (确定性优先, in the spirit of the 第 10 篇 layered
//! self-repair recipe):
//!
//! * **Deterministic results always win.** The analysis renders as an
//!   advisory section *after* "All checks complete!"; it cannot reorder,
//!   rewrite, or suppress the checks above it. `--json` mode is untouched
//!   (machine-readable, CI-safe — no live calls added there).
//! * **Analysis only, never execution.** The model receives findings text
//!   and returns text. Nothing here runs commands, edits config, or
//!   touches the approval surface — the doctor remains prompt-free and
//!   side-effect-free. The safety boundary from P3-8 step 1 carries over:
//!   a validator that could act on its own verdicts would be a validator
//!   you cannot trust.
//! * **Fail silent, never block.** No client resolvable, empty response,
//!   transport error, or timeout (30s) skips the section with a `·` line —
//!   doctor must never get *less* reliable because an LLM was added.

use serde_json::json;

use crate::llm_client::LlmClientHandle;

/// Cap on a single finding's detail text carried into the prompt. Enough
/// for an error message plus the built-in hint; keeps one pathological
/// finding from crowding out the rest.
const DETAIL_CHAR_CAP: usize = 400;

/// Cap on the number of findings serialized into the prompt. Doctor
/// realistically produces a handful; the cap is a belt-and-braces bound
/// on prompt size.
const MAX_FINDINGS: usize = 40;

/// Budget for the analysis response — ~15 lines of grouped advice.
const ANALYSIS_MAX_TOKENS: u32 = 1600;

/// Hard timeout for the advisory call. Doctor is interactive; a hung
/// analysis must not pin the terminal.
const ANALYSIS_TIMEOUT_SECS: u64 = 30;

/// Attention level of a recorded finding. `Ok` lines are not recorded —
/// the collector exists to feed the LLM the things that need reasoning,
/// not to mirror the whole report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FindingStatus {
    Warning,
    Error,
}

impl FindingStatus {
    fn as_str(self) -> &'static str {
        match self {
            FindingStatus::Warning => "warning",
            FindingStatus::Error => "error",
        }
    }
}

/// One doctor warning/error, captured at the same branch that prints it.
#[derive(Debug, Clone)]
pub(crate) struct DoctorFinding {
    pub(crate) section: &'static str,
    pub(crate) status: FindingStatus,
    pub(crate) detail: String,
}

/// Collector handed through `run_doctor`. Recording a finding never
/// changes the printed output — the print sites stay exactly as they
/// were; this only accumulates what the LLM fallback layer will see.
#[derive(Debug, Default)]
pub(crate) struct DoctorFindings {
    entries: Vec<DoctorFinding>,
}

impl DoctorFindings {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn warn(&mut self, section: &'static str, detail: impl Into<String>) {
        self.push(FindingStatus::Warning, section, detail);
    }

    pub(crate) fn error(&mut self, section: &'static str, detail: impl Into<String>) {
        self.push(FindingStatus::Error, section, detail);
    }

    fn push(&mut self, status: FindingStatus, section: &'static str, detail: impl Into<String>) {
        let detail: String = detail.into();
        let detail = crate::utils::truncate_with_ellipsis(&detail, DETAIL_CHAR_CAP, "…");
        self.entries.push(DoctorFinding {
            section,
            status,
            detail,
        });
    }

    pub(crate) fn has_attention(&self) -> bool {
        !self.entries.is_empty()
    }

    /// Human summary for the progress line, e.g. "1 warning, 2 errors".
    pub(crate) fn summary_line(&self) -> String {
        let warnings = self
            .entries
            .iter()
            .filter(|f| f.status == FindingStatus::Warning)
            .count();
        let errors = self.entries.len().saturating_sub(warnings);
        match (warnings, errors) {
            (0, 0) => "no findings".to_string(),
            (w, 0) => format!("{w} warning{}", plural(w)),
            (0, e) => format!("{e} error{}", plural(e)),
            (w, e) => format!("{w} warning{}, {e} error{}", plural(w), plural(e)),
        }
    }

    /// Serialize the findings plus environment context into the prompt
    /// payload. Plain JSON — the analysis contract below tells the model
    /// how to read it. Finding details are home-redacted (`~`) so the
    /// payload does not ship the local username / directory layout to the
    /// LLM endpoint.
    pub(crate) fn to_prompt_payload(
        &self,
        os: &str,
        provider: &str,
        base_url: &str,
        model: &str,
    ) -> String {
        let findings: Vec<serde_json::Value> = self
            .entries
            .iter()
            .take(MAX_FINDINGS)
            .map(|f| {
                json!({
                    "section": f.section,
                    "status": f.status.as_str(),
                    "detail": redact_home(&f.detail),
                })
            })
            .collect();
        json!({
            "environment": {
                "os": os,
                "provider": provider,
                "base_url": redact_home(base_url),
                "model": model,
            },
            "findings": findings,
        })
        .to_string()
    }
}

/// Replace an absolute home-directory prefix with `~` in a string, so the
/// advisory payload carries `~/...` instead of the local username and
/// directory layout. No-op when the home directory cannot be resolved.
fn redact_home(text: &str) -> String {
    let Some(home) = dirs::home_dir() else {
        return text.to_string();
    };
    let home = home.display().to_string();
    if home.is_empty() || !text.contains(&home) {
        return text.to_string();
    }
    text.replace(&home, "~")
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The system prompt for the advisory call. The contract is deliberately
/// explicit about what the analyst may not do — an advisory layer that
/// implies it acted is worse than no advisory layer.
pub(crate) fn analysis_system_prompt() -> String {
    "You are the diagnostic analyst for the codesmith `doctor` command. \
You receive JSON findings collected by deterministic checks (each has \
section, status, detail). The deterministic results are authoritative; \
your analysis is advisory and rendered after them.\n\n\
For each finding whose built-in remediation hint is missing, vague, or \
evidently did not solve the problem: give (a) the most likely root cause \
— prefer explanations that connect multiple findings — and (b) ONE \
concrete next action (an exact command or config edit). Skip findings \
whose printed hint already fully addresses them. You cannot execute \
anything; never claim you ran, fixed, or verified anything. Output plain \
text, at most 15 lines, grouped by finding. If nothing is actionable \
beyond the printed hints, say so in a single line."
        .to_string()
}

/// Resolve the client/model pair for the advisory call: the utility model
/// when configured (the designated cheap brain for side calls), else the
/// main client. `Err` carries the skip reason for the `·` line.
pub(crate) fn resolve_analysis_target(
    config: &crate::config::Config,
) -> Result<(LlmClientHandle, String), String> {
    let main = crate::core::engine::resolve_llm_client(config)
        .map_err(|err| format!("no LLM client resolved: {err}"))?;
    match crate::core::engine::resolve_utility_llm(config, Some(&main)) {
        Some(crate::tools::large_output_router::UtilityLlm { client, model }) => {
            Ok((client, model))
        }
        None => {
            let model = main.model().to_string();
            Ok((main, model))
        }
    }
}

/// Run the advisory analysis. Returns `None` on every skip condition
/// (no findings, empty response, transport error, timeout) — callers
/// print a `·` line and move on. Reports usage through the cost
/// side-channel so the tokens stay visible (workshop-synthesis
/// precedent).
pub(crate) async fn analyze_findings(
    client: &LlmClientHandle,
    model: &str,
    payload: &str,
) -> Option<String> {
    use crate::models::{ContentBlock, Message, MessageRequest};

    let request = MessageRequest {
        model: model.to_string(),
        messages: vec![Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: format!(
                    "Doctor findings to analyze:\n\n{payload}\n\n\
Apply the analysis contract from your instructions."
                ),
                cache_control: None,
            }],
        }],
        max_tokens: ANALYSIS_MAX_TOKENS,
        system: Some(crate::models::SystemPrompt::Text(analysis_system_prompt())),
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: None,
        stream: Some(false),
        temperature: Some(0.2),
        top_p: None,
    };
    let timeout_duration = std::time::Duration::from_secs(ANALYSIS_TIMEOUT_SECS);
    let response =
        match tokio::time::timeout(timeout_duration, client.create_message(request)).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => return None,
            Err(_) => return None,
        };
    codesmith_agent_runtime::cost_status::report(model, &response.usage);
    let text: String = response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_client::mock::{MockLlmClient, canned};

    #[test]
    fn findings_record_and_summarize() {
        let mut findings = DoctorFindings::new();
        assert!(!findings.has_attention());
        assert_eq!(findings.summary_line(), "no findings");

        findings.warn("Updates", "latest release check failed");
        findings.error("API Keys", "active provider key not configured");
        findings.warn("MCP Servers", "npx: command not found");
        assert!(findings.has_attention());
        assert_eq!(findings.summary_line(), "2 warnings, 1 error");
    }

    #[test]
    fn detail_is_capped() {
        let mut findings = DoctorFindings::new();
        findings.error("API Connectivity", "x".repeat(DETAIL_CHAR_CAP * 2));
        let detail_len = findings.entries[0].detail.chars().count();
        assert!(
            detail_len <= DETAIL_CHAR_CAP,
            "detail must be capped, got {detail_len}"
        );
    }

    #[test]
    fn payload_carries_context_and_findings() {
        let mut findings = DoctorFindings::new();
        findings.error(
            "API Connectivity",
            "API connection failed: 401 Unauthorized",
        );
        let payload = findings.to_prompt_payload(
            "macos",
            "deepseek",
            "https://api.deepseek.com",
            "deepseek-chat",
        );
        assert!(payload.contains(r#""os":"macos""#));
        assert!(payload.contains(r#""provider":"deepseek""#));
        assert!(payload.contains(r#""section":"API Connectivity""#));
        assert!(payload.contains(r#""status":"error""#));
        assert!(payload.contains("401 Unauthorized"));
    }

    #[test]
    fn system_prompt_states_the_safety_contract() {
        let prompt = analysis_system_prompt();
        assert!(prompt.contains("advisory"), "must declare advisory status");
        assert!(
            prompt.contains("cannot execute"),
            "must deny execution authority"
        );
        assert!(
            prompt.contains("authoritative"),
            "deterministic results must stay authoritative"
        );
        assert!(prompt.contains("ONE"), "one concrete action per finding");
    }

    #[tokio::test]
    async fn analyze_findings_returns_client_text() {
        let mock = MockLlmClient::new(vec![canned::simple_text_turn(
            "Root cause: stale env key. Action: codesmith auth set --provider deepseek.",
        )]);
        let handle: LlmClientHandle = std::sync::Arc::new(mock);
        let text = analyze_findings(
            &handle,
            "mock-model",
            r#"{"findings":[{"section":"API Keys","status":"error","detail":"401"}]}"#,
        )
        .await
        .expect("analysis text");
        assert!(text.contains("codesmith auth set"));
    }

    #[tokio::test]
    async fn analyze_findings_returns_none_on_transport_error() {
        // No canned response queued → the mock errors → silent skip.
        let handle: LlmClientHandle = std::sync::Arc::new(MockLlmClient::new(Vec::new()));
        assert!(
            analyze_findings(&handle, "mock-model", r#"{"findings":[]}"#)
                .await
                .is_none()
        );
    }
}
