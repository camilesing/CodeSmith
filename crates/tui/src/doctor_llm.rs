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
//!   transport error, or timeout (30s) skips the section with a `·` line
//!   naming the cause where known (transport vs. timeout) — doctor must
//!   never get *less* reliable because an LLM was added.

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
    /// how to read it. Finding details are scrubbed for the outbound trip
    /// ([`redact_outbound`]): home folded to `~`, credential-like runs and
    /// non-home absolute paths masked, so the payload does not ship the
    /// local username, keys, or directory layout to the LLM endpoint.
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
                    "detail": redact_outbound(&f.detail),
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

/// Credential-like assignments (`api_key=…`, `"token": …`,
/// `Authorization: Bearer …`) masked before the payload leaves the
/// machine — transport error text can quote request URLs with query
/// strings or header values verbatim.
///
/// Regex-shape notes (leftmost-first matters): the `authorization` branch
/// runs first and swallows to end-of-line, so `Authorization: Bearer sk-…`
/// redacts the bearer value too — a generic `\S+` after `[:=]` stops at
/// the space and leaked the token (review round 7). The underscore
/// compounds are listed explicitly because `\b` cannot hold between `_`
/// and `token`/`secret` (`_` is a word character). The compounds allow
/// whitespace separators too (`api[\s_-]?key`): the most common provider
/// phrasing is "invalid API key: sk-…", and over-masking is the safe
/// direction for an outbound scrubber.
fn redact_credentials(text: &str) -> String {
    static CREDENTIAL_RUN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = CREDENTIAL_RUN.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)\bauthorization\b["']?\s*[:=][^\n]*|\b(?:api[\s_-]?key|access[\s_-]?token|refresh[\s_-]?token|client[\s_-]?secret|token|secret|password)["']?\s*[:=]\s*\S+|\bbearer\s+\S+"#,
        )
        .expect("static credential regex compiles")
    });
    re.replace_all(text, "<redacted>").to_string()
}

/// Mask absolute filesystem paths outside the home directory: diagnostic
/// text can quote workspace roots under other mount points (`/etc/…`,
/// `/workspaces/…`, `C:\…`, and Windows `\workspaces\…` / UNC
/// `\\server\share\…` shapes). Applied per whitespace token after
/// [`redact_home`] folded the home prefix to `~`, so any remaining token
/// that starts with `/`, a drive letter, or `\` is a non-home absolute
/// path (leading `//` stays excluded — protocol-relative URLs). Over-
/// masking is the safe direction for an outbound scrubber. Whitespace is
/// normalized (tokens rejoined with single spaces).
fn redact_absolute_paths(text: &str) -> String {
    text.split_whitespace()
        .map(|tok| {
            let is_unix_abs =
                (tok.starts_with('/') && !tok.starts_with("//")) || tok.starts_with('\\');
            let is_windows_abs = {
                let bytes = tok.as_bytes();
                bytes.len() >= 3
                    && bytes[0].is_ascii_alphabetic()
                    && bytes[1] == b':'
                    && (bytes[2] == b'\\' || bytes[2] == b'/')
            };
            if is_unix_abs || is_windows_abs {
                "<path>"
            } else {
                tok
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Outbound scrubber for finding details: home-fold (`~`), then mask
/// credential-like runs, then non-home absolute paths.
///
/// Known limitations (accepted): URL hosts and query strings embedded in
/// error text stay visible unless they match the credential pattern, and
/// the env block's `base_url` is sent home-folded but intact — endpoint
/// identity is part of what the analysis reasons about, and the send
/// itself is announced and opt-in via `[doctor] llm_fallback`
/// (documented in `config.example.toml`).
fn redact_outbound(text: &str) -> String {
    redact_absolute_paths(&redact_credentials(&redact_home(text)))
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
/// main client. `Err` carries the skip reason for the `·` line. The main
/// client resolves lossily — a dedicated `[utility_model]` (its own
/// provider + key) must still serve the analysis when the main provider's
/// key is the broken thing doctor is diagnosing; only when both are
/// unavailable does this fail.
pub(crate) fn resolve_analysis_target(
    config: &crate::config::Config,
) -> Result<(LlmClientHandle, String), String> {
    let main_result = crate::core::engine::resolve_llm_client(config);
    // Lossy by design (see doc above), but keep the root cause — a
    // diagnostic command's skip line should say WHY the stack is broken
    // ("no provider factory registered for 'x'"), not just that it is.
    let main_err = main_result.as_ref().err().map(ToString::to_string);
    let main = main_result.ok();
    if let Some(crate::tools::large_output_router::UtilityLlm { client, model }) =
        crate::core::engine::resolve_utility_llm(config, main.as_ref())
    {
        return Ok((client, model));
    }
    match main {
        Some(main) => {
            let model = main.model().to_string();
            Ok((main, model))
        }
        None => Err(match main_err {
            Some(cause) => {
                format!("no LLM client resolved (main and utility both unavailable; main: {cause})")
            }
            None => "no LLM client resolved (main and utility both unavailable)".to_string(),
        }),
    }
}

/// Run the advisory analysis. `Ok(None)` on empty response; `Err(reason)`
/// names the transport error vs. the timeout so the caller's `·` skip line
/// can tell them apart — callers skip the section either way (fail silent,
/// never block). Reports usage through the cost side-channel so the tokens
/// stay visible (workshop-synthesis precedent).
pub(crate) async fn analyze_findings(
    client: &LlmClientHandle,
    model: &str,
    payload: &str,
) -> Result<Option<String>, String> {
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
            Ok(Err(err)) => return Err(format!("request failed: {err}")),
            Err(_) => return Err(format!("timed out after {ANALYSIS_TIMEOUT_SECS}s")),
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
        Ok(None)
    } else {
        Ok(Some(text.to_string()))
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
    fn outbound_details_mask_credentials_and_non_home_paths() {
        let mut findings = DoctorFindings::new();
        findings.error(
            "API Connectivity",
            "GET https://gw.internal/v1 failed at /workspaces/proj with api_key=sk-live-123",
        );
        let payload = findings.to_prompt_payload("linux", "openai", "https://gw.internal", "gpt");
        assert!(!payload.contains("sk-live-123"), "{payload}");
        assert!(payload.contains("<redacted>"), "{payload}");
        assert!(!payload.contains("/workspaces"), "{payload}");
        // URL hosts stay visible by documented design (the analysis needs
        // endpoint identity).
        assert!(payload.contains("gw.internal"), "{payload}");
    }

    #[test]
    fn outbound_details_mask_bearer_headers_and_underscore_credentials() {
        // Round 7: the old leftmost-first alternation matched the
        // `authorization` branch whose `\S+` stopped at the space — only
        // "Bearer" was redacted and `sk-header-9` leaked. The underscore
        // compounds were unreachable too (`\b` cannot hold between `_` and
        // `token`/`secret`).
        let mut findings = DoctorFindings::new();
        findings.error("Auth", "Authorization: Bearer sk-header-9 rejected");
        findings.error(
            "Auth",
            "handshake failed: access_token=t1 client_secret=c1 refresh_token=r1",
        );
        // Round 9: the space-separated compound shapes ("invalid API key:
        // sk-…") — the most common provider phrasing — fell through every
        // alternative before the separator class allowed whitespace.
        findings.error("Auth", "invalid API key: sk-space-7 (revoked)");
        findings.error("Auth", "bad client secret: cs-space-2");
        let payload = findings.to_prompt_payload("linux", "openai", "https://gw.internal", "gpt");
        for secret in ["sk-header-9", "t1", "c1", "r1", "sk-space-7", "cs-space-2"] {
            assert!(
                !payload.contains(secret),
                "credential leaked into outbound payload: {secret} in {payload}"
            );
        }
        assert!(payload.contains("<redacted>"), "{payload}");
    }

    #[test]
    fn outbound_details_mask_backslash_and_unc_paths() {
        // Windows path shapes `path.display()` emits on that platform:
        // root-without-prefix (`\workspaces\proj`) and UNC
        // (`\\server\share\log`) both leaked under the `/`-only check.
        let mut findings = DoctorFindings::new();
        findings.error(
            "Filesystem",
            "read failed at \\workspaces\\proj and \\\\server\\share\\log",
        );
        let payload = findings.to_prompt_payload("windows", "openai", "https://gw.internal", "gpt");
        assert!(!payload.contains("workspaces"), "{payload}");
        assert!(!payload.contains("server"), "{payload}");
        // Protocol-relative URLs stay visible (host identity by design).
        let mut findings = DoctorFindings::new();
        findings.error("Net", "redirected to //cdn.example.com/x");
        let payload = findings.to_prompt_payload("linux", "openai", "https://gw.internal", "gpt");
        assert!(payload.contains("//cdn.example.com/x"), "{payload}");
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
        .expect("transport ok")
        .expect("analysis text");
        assert!(text.contains("codesmith auth set"));
    }

    #[tokio::test]
    async fn analyze_findings_errors_carry_reason() {
        // No canned response queued → the mock errors → the reason must
        // surface (transport vs. timeout) instead of a silent None.
        let handle: LlmClientHandle = std::sync::Arc::new(MockLlmClient::new(Vec::new()));
        let reason = analyze_findings(&handle, "mock-model", r#"{"findings":[]}"#)
            .await
            .expect_err("transport error must surface as Err");
        assert!(reason.starts_with("request failed:"), "{reason}");
    }
}
