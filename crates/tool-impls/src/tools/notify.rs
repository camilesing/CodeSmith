//! `notify` tool — model-callable desktop notification (#1322).
//!
//! Routes through the host-injected [`NotifierHost`] (see
//! `codesmith_agent_runtime::host_services`), which picks the terminal
//! protocol — OSC 9 for known capable terminals, BEL fallback on macOS /
//! Linux, `MessageBeep` on Windows when explicitly opted in — and wraps for
//! tmux. The tool itself is host-agnostic: the model decides when to fire
//! and the host decides how it lands.
//!
//! Intended for "long task done, come back" beats and sub-agent-completion
//! pings, not chatter. The host auto-suppresses when
//! `[notifications].method = "off"`. Output messages are length-capped so a
//! runaway model can't paint a paragraph into the terminal title bar.

use async_trait::async_trait;
use serde_json::{Value, json};

use codesmith_agent_runtime::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_str, required_str,
};

/// Maximum chars passed through for the title — keeps the OSC 9 escape
/// reasonable on terminals that wrap long titles awkwardly.
const NOTIFY_TITLE_CAP: usize = 80;
/// Maximum chars passed through for the body. Most receivers truncate
/// past ~120, so 200 leaves headroom while still bounded.
const NOTIFY_BODY_CAP: usize = 200;

/// Tool that fires a single desktop notification.
pub struct NotifyTool;

#[async_trait]
impl ToolSpec for NotifyTool {
    fn name(&self) -> &'static str {
        "notify"
    }

    fn description(&self) -> &'static str {
        "Fire a single desktop notification (OSC 9 / terminal bell). Use \
         sparingly — only when a long-running task completes, when a turn \
         was waiting on a remote operation that just finished, or when \
         the user genuinely needs to come back to the terminal. Pass a \
         short `title` and an optional `body`. Do NOT use this for \
         routine progress updates, conversational acknowledgements, or \
         confirmation that the model is alive — that's noise. The user \
         can disable notifications entirely via \
         `[notifications].method = \"off\"` in `~/.deepseek/config.toml`; \
         when disabled this tool is a silent no-op."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Short notification title (≤ 80 chars after truncation). Required."
                },
                "body": {
                    "type": "string",
                    "description": "Optional longer body (≤ 200 chars after truncation)."
                }
            },
            "required": ["title"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        // No filesystem or shell side effects; the only output is a single
        // terminal-escape write to stdout. Mark as ReadOnly so the
        // approval-requirement default is `Auto` and the tool routes
        // through without prompting.
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        let title_raw = required_str(&input, "title")?;
        let body_raw = optional_str(&input, "body").unwrap_or("");

        // Char-bounded truncation (not byte-bounded) so we don't slice
        // through a multi-byte sequence and emit invalid UTF-8 to the
        // terminal.
        let title: String = title_raw.chars().take(NOTIFY_TITLE_CAP).collect();
        let body: String = body_raw.chars().take(NOTIFY_BODY_CAP).collect();
        let title = title.trim();
        let body = body.trim();

        if title.is_empty() {
            return Err(ToolError::execution_failed("title must not be empty"));
        }

        let msg = if body.is_empty() {
            title.to_string()
        } else {
            format!("{title}: {body}")
        };

        // Route through the host-injected `NotifierHost` rather than the
        // TUI's `tui::notifications::notify_done` directly — this keeps the
        // tool portable across hosts (TUI today, app-server tomorrow). The
        // host resolves the terminal protocol (OSC 9 / BEL / MessageBeep)
        // and wraps for tmux; the "threshold = 0 so it always fires" and
        // the 1s elapsed gate that the TUI impl applied are host-side
        // concerns now.
        let notifier = ctx.runtime.notifier.as_ref().ok_or_else(|| {
            ToolError::execution_failed(
                "notify tool is not available: no notifier attached",
            )
        })?;
        notifier.notify_done(&msg);

        Ok(ToolResult::success(format!("notified: {title}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_agent_runtime::host_services::NotifierHost;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    /// Test notifier that records the last message it received, so the
    /// success-path tests can assert the host actually got pinged (not just
    /// that the tool returned `Ok`).
    #[derive(Default)]
    struct TestNotifier {
        last: Mutex<Option<String>>,
    }

    impl NotifierHost for TestNotifier {
        fn notify_done(&self, msg: &str) {
            *self.last.lock().unwrap() = Some(msg.to_string());
        }
    }

    /// Context with no notifier attached — mirrors the "host forgot to wire
    /// the notifier" case and is used by tests that fail before reaching it.
    fn ctx() -> ToolContext {
        ToolContext::new(Path::new("."))
    }

    /// Context with a test notifier attached. The caller keeps its own
    /// `Arc<TestNotifier>` so it can inspect what landed after `execute`.
    fn ctx_with_notifier(notifier: &Arc<TestNotifier>) -> ToolContext {
        let mut c = ToolContext::new(Path::new("."));
        let dyn_n: Arc<dyn NotifierHost> = notifier.clone();
        c.runtime.notifier = Some(dyn_n);
        c
    }

    #[tokio::test]
    async fn rejects_missing_title() {
        let notifier = Arc::new(TestNotifier::default());
        let err = NotifyTool
            .execute(json!({}), &ctx_with_notifier(&notifier))
            .await
            .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("title"), "{err}");
        // Never reached the notifier.
        assert!(notifier.last.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn rejects_empty_title_after_trim() {
        let notifier = Arc::new(TestNotifier::default());
        let err = NotifyTool
            .execute(json!({"title": "   "}), &ctx_with_notifier(&notifier))
            .await
            .unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("must not be empty"),
            "{err}"
        );
        assert!(notifier.last.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn errors_when_no_notifier_attached() {
        // Reaching the notify call without a host-injected notifier is a
        // host wiring bug, not a model error — surface it loudly.
        let err = NotifyTool
            .execute(json!({"title": "done"}), &ctx())
            .await
            .unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("notifier"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn truncates_title_to_cap() {
        let notifier = Arc::new(TestNotifier::default());
        let long = "x".repeat(500);
        let result = NotifyTool
            .execute(json!({"title": long}), &ctx_with_notifier(&notifier))
            .await
            .expect("ok");
        // Confirmation message echoes the *truncated* title.
        let echo_x_count = result.content.matches('x').count();
        assert_eq!(echo_x_count, NOTIFY_TITLE_CAP);
        // Notifier was invoked with the truncated title (no body).
        let landed = notifier.last.lock().unwrap().clone();
        assert_eq!(landed, Some("x".repeat(NOTIFY_TITLE_CAP)));
    }

    #[tokio::test]
    async fn accepts_body_optional() {
        let notifier = Arc::new(TestNotifier::default());
        let result = NotifyTool
            .execute(
                json!({"title": "done", "body": "tests pass"}),
                &ctx_with_notifier(&notifier),
            )
            .await
            .expect("ok");
        assert!(result.success);
        assert!(result.content.contains("done"));
        // Body is appended to the title for the notifier message.
        let landed = notifier.last.lock().unwrap().clone();
        assert_eq!(landed.as_deref(), Some("done: tests pass"));
    }

    #[tokio::test]
    async fn safe_against_multibyte_truncation() {
        // Construct a title whose char-count is below the cap but whose
        // byte-count would be above a naive byte cap; assert no panic
        // and the success-content roundtrips the title intact.
        let notifier = Arc::new(TestNotifier::default());
        let title: String = "我".repeat(30); // 30 chars × 3 bytes = 90 bytes, < 80 chars cap (well, == 30 chars)
        let result = NotifyTool
            .execute(
                json!({"title": title.clone()}),
                &ctx_with_notifier(&notifier),
            )
            .await
            .expect("ok");
        assert!(result.content.contains(&title));
        let landed = notifier.last.lock().unwrap().clone();
        assert_eq!(landed, Some(title));
    }

    #[test]
    fn schema_exposes_title_and_body_fields() {
        let schema = NotifyTool.input_schema();
        let props = schema.get("properties").unwrap();
        assert!(props.get("title").is_some());
        assert!(props.get("body").is_some());
        let required = schema.get("required").unwrap().as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("title")));
        assert!(!required.iter().any(|v| v.as_str() == Some("body")));
    }
}
