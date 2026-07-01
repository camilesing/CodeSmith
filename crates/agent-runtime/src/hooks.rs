//! Hook data types and the [`HookHost`] trait.
//!
//! The hook *value types* (`HookEvent`, `HookContext`, `HookResult`,
//! `MessageSubmitOutcome`) and the [`HookHost`] abstraction were relocated
//! here from the TUI's `hooks` module so the runtime can invoke lifecycle
//! hooks without depending on the TUI's concrete `HookExecutor` (which spawns
//! processes and reads the host `Config`). The TUI keeps its `HookExecutor`
//! and implements [`HookHost`] for it; the runtime holds `Arc<dyn HookHost>`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Events that can trigger hook execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookEvent {
    #[serde(rename = "session_start")]
    SessionStart,
    #[serde(rename = "session_end")]
    SessionEnd,
    #[serde(rename = "message_submit")]
    MessageSubmit,
    #[serde(rename = "tool_call_before", alias = "pre_tool_use")]
    ToolCallBefore,
    #[serde(rename = "tool_call_after", alias = "post_tool_use")]
    ToolCallAfter,
    #[serde(rename = "mode_change")]
    ModeChange,
    #[serde(rename = "on_error")]
    OnError,
    /// Immediately before each `exec_shell` invocation. The hook's stdout is
    /// parsed as `KEY=VALUE\n` lines and merged onto the shell command's
    /// environment (#456).
    #[serde(rename = "shell_env")]
    ShellEnv,
    #[serde(rename = "task_created")]
    TaskCreated,
    #[serde(rename = "task_completed")]
    TaskCompleted,
    /// Immediately before context compaction. The hook's stdout is collected
    /// as "context to preserve" and merged into the compaction summary (#485).
    #[serde(rename = "pre_compact")]
    PreCompact,
}

impl HookEvent {
    /// String representation for environment variables.
    pub fn as_str(self) -> &'static str {
        match self {
            HookEvent::SessionStart => "session_start",
            HookEvent::SessionEnd => "session_end",
            HookEvent::MessageSubmit => "message_submit",
            HookEvent::ToolCallBefore => "tool_call_before",
            HookEvent::ToolCallAfter => "tool_call_after",
            HookEvent::ModeChange => "mode_change",
            HookEvent::OnError => "on_error",
            HookEvent::ShellEnv => "shell_env",
            HookEvent::TaskCreated => "task_created",
            HookEvent::TaskCompleted => "task_completed",
            HookEvent::PreCompact => "pre_compact",
        }
    }
}

/// Context passed to hook execution.
#[derive(Debug, Clone, Default)]
pub struct HookContext {
    /// Tool name (for ToolCallBefore/After).
    pub tool_name: Option<String>,
    /// Tool arguments as JSON string.
    pub tool_args: Option<String>,
    /// Tool result output (truncated).
    pub tool_result: Option<String>,
    /// Tool exit code if applicable.
    pub tool_exit_code: Option<i32>,
    /// Whether tool succeeded.
    pub tool_success: Option<bool>,
    /// Current mode.
    pub mode: Option<String>,
    /// Previous mode (for `ModeChange`).
    pub previous_mode: Option<String>,
    /// Ephemeral per-construction telemetry session id (finding 5c). Emitted
    /// to hooks as `DEEPSEEK_SESSION_ID`. Regenerated on each engine
    /// construction and **not** correlatable across restarts — use
    /// [`thread_id`](Self::thread_id) for that.
    pub session_id: Option<String>,
    /// Persistent conversation thread id (the durable resume id). Emitted to
    /// hooks as `DEEPSEEK_THREAD_ID` so hook authors can correlate events
    /// across restarts, unlike the ephemeral [`session_id`](Self::session_id).
    pub thread_id: Option<String>,
    /// User message content.
    pub message: Option<String>,
    /// Error message (for `OnError`).
    pub error_message: Option<String>,
    /// Workspace path.
    pub workspace: Option<PathBuf>,
    /// Current model name.
    pub model: Option<String>,
    /// Total tokens used.
    pub total_tokens: Option<u32>,
    /// Session cost in USD.
    pub session_cost: Option<f64>,
    /// Task ID (for TaskCreated/TaskCompleted).
    pub task_id: Option<String>,
    /// Task subject (for TaskCreated/TaskCompleted).
    pub task_subject: Option<String>,
    /// Task status (for TaskCreated/TaskCompleted).
    pub task_status: Option<String>,
}

impl HookContext {
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(dead_code)]
    pub fn with_tool_name(mut self, name: &str) -> Self {
        self.tool_name = Some(name.to_string());
        self
    }

    #[allow(dead_code)]
    pub fn with_tool_args(mut self, args: &serde_json::Value) -> Self {
        self.tool_args = Some(args.to_string());
        self
    }

    #[allow(dead_code)]
    pub fn with_tool_result(mut self, result: &str, success: bool, exit_code: Option<i32>) -> Self {
        self.tool_result = Some(result.to_string());
        self.tool_success = Some(success);
        self.tool_exit_code = exit_code;
        self
    }

    #[allow(dead_code)]
    pub fn with_mode(mut self, mode: &str) -> Self {
        self.mode = Some(mode.to_string());
        self
    }

    pub fn with_previous_mode(mut self, mode: &str) -> Self {
        self.previous_mode = Some(mode.to_string());
        self
    }

    #[allow(dead_code)]
    pub fn with_workspace(mut self, path: PathBuf) -> Self {
        self.workspace = Some(path);
        self
    }

    pub fn with_model(mut self, model: &str) -> Self {
        self.model = Some(model.to_string());
        self
    }

    pub fn with_session_id(mut self, session_id: &str) -> Self {
        self.session_id = Some(session_id.to_string());
        self
    }

    pub fn with_thread_id(mut self, thread_id: &str) -> Self {
        self.thread_id = Some(thread_id.to_string());
        self
    }

    #[allow(dead_code)]
    pub fn with_message(mut self, message: &str) -> Self {
        self.message = Some(message.to_string());
        self
    }

    #[allow(dead_code)]
    pub fn with_error(mut self, error: &str) -> Self {
        self.error_message = Some(error.to_string());
        self
    }

    pub fn with_tokens(mut self, tokens: u32) -> Self {
        self.total_tokens = Some(tokens);
        self
    }

    #[allow(dead_code)]
    pub fn with_cost(mut self, cost: f64) -> Self {
        self.session_cost = Some(cost);
        self
    }

    #[allow(dead_code)]
    pub fn with_task_id(mut self, id: &str) -> Self {
        self.task_id = Some(id.to_string());
        self
    }

    #[allow(dead_code)]
    pub fn with_task_subject(mut self, subject: &str) -> Self {
        self.task_subject = Some(subject.to_string());
        self
    }

    #[allow(dead_code)]
    pub fn with_task_status(mut self, status: &str) -> Self {
        self.task_status = Some(status.to_string());
        self
    }

    /// Convert to environment variables.
    pub fn to_env_vars(&self) -> HashMap<String, String> {
        let mut env = HashMap::new();

        if let Some(ref name) = self.tool_name {
            env.insert("DEEPSEEK_TOOL_NAME".to_string(), name.clone());
        }
        if let Some(ref args) = self.tool_args {
            env.insert("DEEPSEEK_TOOL_ARGS".to_string(), args.clone());
        }
        if let Some(ref result) = self.tool_result {
            // Truncate result to 10KB to avoid environment variable size limits.
            let truncated = if result.len() > 10000 {
                let safe_end = result
                    .char_indices()
                    .take_while(|(i, _)| *i < 10000)
                    .last()
                    .map(|(i, c)| i + c.len_utf8())
                    .unwrap_or(0);
                format!("{}...[truncated]", &result[..safe_end])
            } else {
                result.clone()
            };
            env.insert("DEEPSEEK_TOOL_RESULT".to_string(), truncated);
        }
        if let Some(code) = self.tool_exit_code {
            env.insert("DEEPSEEK_TOOL_EXIT_CODE".to_string(), code.to_string());
        }
        if let Some(success) = self.tool_success {
            env.insert("DEEPSEEK_TOOL_SUCCESS".to_string(), success.to_string());
        }
        if let Some(ref mode) = self.mode {
            env.insert("DEEPSEEK_MODE".to_string(), mode.clone());
        }
        if let Some(ref prev) = self.previous_mode {
            env.insert("DEEPSEEK_PREVIOUS_MODE".to_string(), prev.clone());
        }
        if let Some(ref session_id) = self.session_id {
            env.insert("DEEPSEEK_SESSION_ID".to_string(), session_id.clone());
        }
        if let Some(ref thread_id) = self.thread_id {
            env.insert("DEEPSEEK_THREAD_ID".to_string(), thread_id.clone());
        }
        if let Some(ref message) = self.message {
            let truncated = if message.len() > 5000 {
                let safe_end = message
                    .char_indices()
                    .take_while(|(i, _)| *i < 5000)
                    .last()
                    .map(|(i, c)| i + c.len_utf8())
                    .unwrap_or(0);
                format!("{}...[truncated]", &message[..safe_end])
            } else {
                message.clone()
            };
            env.insert("DEEPSEEK_MESSAGE".to_string(), truncated);
        }
        if let Some(ref error) = self.error_message {
            env.insert("DEEPSEEK_ERROR".to_string(), error.clone());
        }
        if let Some(ref ws) = self.workspace {
            env.insert("DEEPSEEK_WORKSPACE".to_string(), ws.display().to_string());
        }
        if let Some(ref model) = self.model {
            env.insert("DEEPSEEK_MODEL".to_string(), model.clone());
        }
        if let Some(tokens) = self.total_tokens {
            env.insert("DEEPSEEK_TOTAL_TOKENS".to_string(), tokens.to_string());
        }
        if let Some(cost) = self.session_cost {
            env.insert("DEEPSEEK_SESSION_COST".to_string(), format!("{cost:.6}"));
        }
        if let Some(ref task_id) = self.task_id {
            env.insert("DEEPSEEK_TASK_ID".to_string(), task_id.clone());
        }
        if let Some(ref task_subject) = self.task_subject {
            env.insert("DEEPSEEK_TASK_SUBJECT".to_string(), task_subject.clone());
        }
        if let Some(ref task_status) = self.task_status {
            env.insert("DEEPSEEK_TASK_STATUS".to_string(), task_status.clone());
        }

        env
    }
}

/// Result of a hook execution.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HookResult {
    /// Hook name (if specified).
    pub name: Option<String>,
    /// Whether the hook succeeded.
    pub success: bool,
    /// Exit code from the hook command.
    pub exit_code: Option<i32>,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
    /// Time taken to execute.
    pub duration: Duration,
    /// Error message if execution failed.
    pub error: Option<String>,
}

/// Result of running mutable `message_submit` hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageSubmitOutcome {
    /// No hook changed the submitted text.
    Unchanged { warning: Option<String> },
    /// One or more hooks replaced the submitted text.
    Replaced {
        text: String,
        warning: Option<String>,
    },
    /// A hook intentionally blocked the submission.
    Blocked { reason: String },
}

impl MessageSubmitOutcome {
    pub fn unchanged() -> Self {
        Self::Unchanged { warning: None }
    }

    pub fn replaced(text: String) -> Self {
        Self::Replaced {
            text,
            warning: None,
        }
    }

    pub fn with_warning(self, warning: Option<String>) -> Self {
        match self {
            Self::Unchanged { .. } => Self::Unchanged { warning },
            Self::Replaced { text, .. } => Self::Replaced { text, warning },
            Self::Blocked { reason } => Self::Blocked { reason },
        }
    }

    pub fn warning(&self) -> Option<&str> {
        match self {
            Self::Unchanged { warning } | Self::Replaced { warning, .. } => warning.as_deref(),
            Self::Blocked { .. } => None,
        }
    }
}

/// Host-supplied hook execution surface.
///
/// Implemented by the TUI's `HookExecutor` (synchronous, process-spawning).
/// The runtime holds `Arc<dyn HookHost>` and invokes lifecycle hooks through
/// it, keeping process-spawning and `Config` reading on the host side.
pub trait HookHost: Send + Sync {
    /// Run all hooks for `event`, returning each result.
    fn execute(&self, event: HookEvent, context: &HookContext) -> Vec<HookResult>;
    /// Run the `PreCompact` hook and return its stdout as context to preserve.
    fn execute_pre_compact_hook(&self, context: &HookContext) -> Option<String>;
    /// Run mutable `MessageSubmit` hooks that may replace the submitted text.
    fn execute_message_submit_transform(
        &self,
        context: &HookContext,
        original_text: &str,
    ) -> MessageSubmitOutcome;
    /// Whether any hook is configured for `event`.
    fn has_hooks_for_event(&self, event: HookEvent) -> bool;
    /// Whether hook execution is enabled at all.
    fn is_enabled(&self) -> bool;
    /// Session ID, used when building `HookContext` for tool-call hooks.
    fn session_id(&self) -> &str;
    /// Collect ephemeral `KEY=VALUE` shell env vars from `ShellEnv` hooks.
    /// Used by the `exec_shell` tool to inject per-skill credentials, PATH
    /// adjustments, etc. Failures contribute no vars (logged by the host).
    fn collect_shell_env(&self, context: &HookContext) -> HashMap<String, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_vars_split_session_and_thread_id() {
        let ctx = HookContext::new()
            .with_session_id("ephemeral-123")
            .with_thread_id("thread-abc");
        let env = ctx.to_env_vars();
        // DEEPSEEK_SESSION_ID carries the ephemeral telemetry id.
        assert_eq!(env.get("DEEPSEEK_SESSION_ID"), Some(&"ephemeral-123".to_string()));
        // DEEPSEEK_THREAD_ID carries the persistent resume thread id.
        assert_eq!(env.get("DEEPSEEK_THREAD_ID"), Some(&"thread-abc".to_string()));
    }

    #[test]
    fn thread_id_env_var_omitted_when_unset() {
        let ctx = HookContext::new().with_session_id("ephemeral-only");
        let env = ctx.to_env_vars();
        assert_eq!(env.get("DEEPSEEK_SESSION_ID"), Some(&"ephemeral-only".to_string()));
        assert!(!env.contains_key("DEEPSEEK_THREAD_ID"));
    }
}
