//! Hooks system for `DeepSeek` CLI
//!
//! Provides lifecycle hooks that execute user-defined shell commands at:
//! - Session start/end
//! - Tool call before/after

//! - Mode changes
//! - Message submission
//! - Error events
//!
//! Configuration is done via `[[hooks.hooks]]` in config.toml.

// Note: anyhow is available if needed for future error handling
#[allow(unused_imports)]
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

// Hook value types (`HookEvent`, `HookContext`, `HookResult`,
// `MessageSubmitOutcome`) and the `HookHost` trait live in
// `codesmith-agent-runtime::hooks`; re-exported here so existing
// `crate::hooks::HookEvent` etc. paths keep resolving.
pub use codesmith_agent_runtime::hooks::{
    HookContext, HookEvent, HookHost, HookResult, MessageSubmitOutcome,
};

/// Condition for when a hook should run
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[derive(Default)]
pub enum HookCondition {
    /// Always run this hook
    #[default]
    Always,
    /// Only run for specific tool names
    ToolName {
        /// Tool name to match (e.g., "`exec_shell`", "`write_file`")
        name: String,
    },
    /// Only run for specific tool categories
    ToolCategory {
        /// Category: "safe", "`file_write`", "shell"
        category: String,
    },
    /// Only run in specific modes
    Mode {
        /// Mode: "plan", "agent", "yolo"
        mode: String,
    },
    /// Only run when exit code matches (for `ToolCallAfter`)
    ExitCode {
        /// Exit code to match
        code: i32,
    },
    /// Combine multiple conditions with AND
    All { conditions: Vec<HookCondition> },
    /// Combine multiple conditions with OR
    Any { conditions: Vec<HookCondition> },
}

/// A single hook definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hook {
    /// The event that triggers this hook
    pub event: HookEvent,

    /// Shell command to execute (platform shell: `sh -c` on Unix, `cmd /C` on Windows)
    pub command: String,

    /// Optional condition for when this hook should run
    #[serde(default)]
    pub condition: Option<HookCondition>,

    /// Timeout in seconds (default: 30). The executor-level
    /// `default_timeout_secs` override — when set — always takes
    /// precedence over this value (table-over-hook precedence, as
    /// documented in `docs/HOOKS.md`).
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    /// Run in background (don't wait for completion)
    #[serde(default)]
    pub background: bool,

    /// Continue if this hook fails (default: true)
    #[serde(default = "default_continue_on_error")]
    pub continue_on_error: bool,

    /// Optional name for logging/debugging
    #[serde(default)]
    pub name: Option<String>,
}

fn default_timeout() -> u64 {
    30
}
fn default_continue_on_error() -> bool {
    true
}

impl Hook {
    /// Create a new hook with minimal configuration
    #[allow(dead_code)] // Public builder API, used in tests
    pub fn new(event: HookEvent, command: &str) -> Self {
        Self {
            event,
            command: command.to_string(),
            condition: None,
            timeout_secs: 30,
            background: false,
            continue_on_error: true,
            name: None,
        }
    }

    /// Builder: set condition
    #[allow(dead_code)] // Public builder API, used in tests
    pub fn with_condition(mut self, condition: HookCondition) -> Self {
        self.condition = Some(condition);
        self
    }

    /// Builder: set timeout
    #[allow(dead_code)] // Public builder API, used in tests
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// Builder: run in background
    #[allow(dead_code)] // Public builder API, used in tests
    pub fn background(mut self) -> Self {
        self.background = true;
        self
    }

    /// Builder: set name
    #[allow(dead_code)] // Public builder API, used in tests
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }
}

/// Configuration for hooks (loaded from config.toml)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HooksConfig {
    /// List of hooks to execute
    #[serde(default)]
    pub hooks: Vec<Hook>,

    /// Global enable/disable for all hooks
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Global timeout override. When set it takes precedence over every
    /// hook's `timeout_secs` (table-over-hook precedence, documented in
    /// `docs/HOOKS.md`); when unset, each hook's own timeout applies.
    #[serde(default)]
    pub default_timeout_secs: Option<u64>,

    /// Working directory for hook execution (default: workspace)
    #[serde(default)]
    pub working_dir: Option<PathBuf>,
}

fn default_enabled() -> bool {
    true
}

impl HooksConfig {
    /// Get hooks for a specific event
    pub fn hooks_for_event(&self, event: HookEvent) -> Vec<&Hook> {
        if !self.enabled {
            return Vec::new();
        }
        self.hooks.iter().filter(|h| h.event == event).collect()
    }

    /// Check if hooks are configured and enabled
    #[allow(dead_code)] // Public API for hook system consumers
    pub fn has_hooks(&self) -> bool {
        self.enabled && !self.hooks.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MessageSubmitStdout {
    Unchanged,
    Replaced(String),
    Invalid(String),
}

/// Executor for running hooks
#[derive(Debug, Clone)]
pub struct HookExecutor {
    config: HooksConfig,
    default_working_dir: PathBuf,
    session_id: String,
}

/// Trait-impl bridge: the runtime holds `Arc<dyn HookHost>` and invokes
/// lifecycle hooks through this surface; `HookExecutor` delegates to its
/// inherent (process-spawning) methods. Fully-qualified call syntax
/// (`HookExecutor::execute(self, …)`) disambiguates from the trait method
/// of the same name.
impl codesmith_agent_runtime::hooks::HookHost for HookExecutor {
    fn execute(&self, event: HookEvent, context: &HookContext) -> Vec<HookResult> {
        HookExecutor::execute(self, event, context)
    }

    fn execute_pre_compact_hook(&self, context: &HookContext) -> Option<String> {
        HookExecutor::execute_pre_compact_hook(self, context)
    }

    fn execute_message_submit_transform(
        &self,
        context: &HookContext,
        original_text: &str,
    ) -> MessageSubmitOutcome {
        HookExecutor::execute_message_submit_transform(self, context, original_text)
    }

    fn has_hooks_for_event(&self, event: HookEvent) -> bool {
        HookExecutor::has_hooks_for_event(self, event)
    }

    fn is_enabled(&self) -> bool {
        HookExecutor::is_enabled(self)
    }

    fn session_id(&self) -> &str {
        HookExecutor::session_id(self)
    }

    fn collect_shell_env(&self, context: &HookContext) -> HashMap<String, String> {
        HookExecutor::collect_shell_env(self, context)
    }
}

impl HookExecutor {
    fn build_shell_command(command: &str) -> Command {
        #[cfg(windows)]
        {
            let mut cmd = Command::new("cmd");
            cmd.arg("/C").arg(command);
            cmd
        }
        #[cfg(not(windows))]
        {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(command);
            cmd
        }
    }

    /// Put a hook child in its own process group (unix) so a timeout kill
    /// can take the whole tree — `sh -c` alone dying leaves grandchildren
    /// (backgrounded jobs, daemonizers) running. No-op on Windows.
    #[cfg(not(windows))]
    fn own_process_group(cmd: &mut Command) {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    fn own_process_group(_cmd: &mut Command) {}

    /// Create a new `HookExecutor` with configuration
    pub fn new(config: HooksConfig, default_working_dir: PathBuf) -> Self {
        // Generate a session ID
        let session_id = format!("sess_{}", &uuid::Uuid::new_v4().to_string()[..8]);
        Self {
            config,
            default_working_dir,
            session_id,
        }
    }

    /// Create a disabled `HookExecutor` (no hooks will run)
    #[allow(dead_code)] // Used in tests and as convenience constructor
    pub fn disabled() -> Self {
        Self {
            config: HooksConfig {
                enabled: false,
                ..Default::default()
            },
            default_working_dir: PathBuf::from("."),
            session_id: String::new(),
        }
    }

    /// Check if hooks are enabled
    #[allow(dead_code)] // Public API for hook system consumers
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Read-only access to the underlying configuration. Used by
    /// `/hooks` (#460 read-only MVP) so the user can list configured
    /// hooks without reaching for `cat ~/.codesmith/config.toml`.
    pub fn config(&self) -> &HooksConfig {
        &self.config
    }

    /// Get the session ID
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Cheap pre-check: are there any enabled hooks for this event?
    /// Lets call sites avoid building a [`HookContext`] (which allocates
    /// for `workspace`, `model`, `session_id`, …) on every tool call
    /// when the user hasn't configured any hooks. The cost matters
    /// because `ToolCallBefore` / `ToolCallAfter` fire from
    /// `tool_routing.rs` on every tool dispatch (#455).
    #[must_use]
    pub fn has_hooks_for_event(&self, event: HookEvent) -> bool {
        self.config.enabled && self.config.hooks.iter().any(|h| h.event == event)
    }

    /// Run configured `message_submit` hooks as a mutable submit pipeline.
    ///
    /// This is deliberately separate from [`Self::execute`]: most hook events
    /// are observer-only, while `message_submit` has a narrow stdout JSON
    /// contract that can replace or block the submitted text.
    pub fn execute_message_submit_transform(
        &self,
        context: &HookContext,
        original_text: &str,
    ) -> MessageSubmitOutcome {
        if !self.config.enabled {
            return MessageSubmitOutcome::unchanged();
        }

        let hooks = self.config.hooks_for_event(HookEvent::MessageSubmit);
        if hooks.is_empty() {
            return MessageSubmitOutcome::unchanged();
        }

        let mut current_text = original_text.to_string();
        let mut warning = None;

        for hook in hooks {
            let hook_context = context.clone().with_message(&current_text);
            if !self.matches_condition(hook, &hook_context) {
                continue;
            }

            let env_vars = hook_context.to_env_vars();
            if hook.background {
                let _ = self.execute_background(hook, &env_vars);
                continue;
            }

            let payload = message_submit_payload(&hook_context, &current_text);
            let result = self.execute_sync_with_stdin(hook, &env_vars, &payload);

            if result.exit_code == Some(2) {
                return MessageSubmitOutcome::Blocked {
                    reason: message_submit_block_reason(
                        &result,
                        "message_submit hook blocked submission",
                    ),
                };
            }

            if !result.success {
                let label = result.name.as_deref().unwrap_or("(unnamed)");
                tracing::warn!(
                    target: "hooks",
                    hook = label,
                    event = "message_submit",
                    exit_code = ?result.exit_code,
                    duration_ms = result.duration.as_millis() as u64,
                    error = result.error.as_deref().unwrap_or(""),
                    stderr_head = %result.stderr.lines().next().unwrap_or(""),
                    "message_submit hook failed"
                );

                if hook.continue_on_error {
                    warning = message_submit_continue_warning(&result).or(warning);
                    continue;
                }

                return MessageSubmitOutcome::Blocked {
                    reason: message_submit_block_reason(
                        &result,
                        "message_submit hook failed and blocked submission",
                    ),
                };
            }

            match parse_message_submit_stdout(&result.stdout) {
                MessageSubmitStdout::Unchanged => {}
                MessageSubmitStdout::Replaced(text) => {
                    current_text = text;
                }
                MessageSubmitStdout::Invalid(reason) => {
                    tracing::warn!(
                        target: "hooks",
                        hook = result.name.as_deref().unwrap_or("(unnamed)"),
                        event = "message_submit",
                        reason = %reason,
                        "ignored invalid message_submit hook stdout"
                    );
                }
            }
        }

        if current_text == original_text {
            MessageSubmitOutcome::unchanged().with_warning(warning)
        } else {
            MessageSubmitOutcome::replaced(current_text).with_warning(warning)
        }
    }

    /// Run configured `pre_compact` hooks and collect their stdout as
    /// "context to preserve".
    ///
    /// Fires before context compaction (manual, auto, or emergency). Each
    /// matching non-background hook's stdout is concatenated (separated by
    /// `---` rules) into a single preserve-context blob that the compaction
    /// layer merges into its summary so the material survives summarization.
    ///
    /// Deliberately non-blocking: a hook that fails, times out, or exits
    /// non-zero contributes nothing and a `tracing::warn!` lands, but
    /// compaction always proceeds (#485). Background hooks are
    /// fire-and-forget and do not contribute preserve context.
    pub fn execute_pre_compact_hook(&self, context: &HookContext) -> Option<String> {
        if !self.config.enabled {
            return None;
        }
        let hooks = self.config.hooks_for_event(HookEvent::PreCompact);
        if hooks.is_empty() {
            return None;
        }

        let mut preserved: Option<String> = None;
        for hook in hooks {
            if !self.matches_condition(hook, context) {
                continue;
            }
            let env_vars = context.to_env_vars();
            if hook.background {
                let _ = self.execute_background(hook, &env_vars);
                continue;
            }

            let payload = serde_json::json!({
                "hook_event_name": "pre_compact",
                "session_id": context.session_id,
                "thread_id": context.thread_id,
                "workspace": context.workspace.as_ref().map(|p| p.display().to_string()),
                "model": context.model,
                "total_tokens": context.total_tokens,
            });
            let result = self.execute_sync_with_stdin(hook, &env_vars, &payload);

            if !result.success {
                tracing::warn!(
                    target: "hooks",
                    hook = result.name.as_deref().unwrap_or("(unnamed)"),
                    event = "pre_compact",
                    exit_code = ?result.exit_code,
                    duration_ms = result.duration.as_millis() as u64,
                    error = result.error.as_deref().unwrap_or(""),
                    stderr_head = %result.stderr.lines().next().unwrap_or(""),
                    "pre_compact hook failed; contributing no preserve context"
                );
                continue;
            }

            let stdout = result.stdout.trim();
            if stdout.is_empty() {
                continue;
            }
            let is_first = preserved.is_none();
            let buf = preserved.get_or_insert_with(String::new);
            if !is_first {
                buf.push_str("\n\n---\n\n");
            }
            buf.push_str(stdout);
        }

        preserved
    }

    /// Run every `ShellEnv` hook for this context and merge their stdout
    /// (`KEY=VALUE\n` lines) into a single env-var map. Used by the
    /// `exec_shell` tool to inject ephemeral credentials, per-skill PATH
    /// adjustments, etc. (#456). Failures don't abort the shell call —
    /// the hook simply contributes no vars and a `tracing::warn!` lands.
    ///
    /// Each successful hook's keys (NOT values) are written to the audit
    /// log so a session can be reconciled later without leaking the
    /// secret material itself.
    pub fn collect_shell_env(&self, context: &HookContext) -> HashMap<String, String> {
        let mut merged: HashMap<String, String> = HashMap::new();
        if !self.config.enabled {
            return merged;
        }
        let hooks = self.config.hooks_for_event(HookEvent::ShellEnv);
        if hooks.is_empty() {
            return merged;
        }
        let env_vars = context.to_env_vars();
        for hook in hooks {
            if !self.matches_condition(hook, context) {
                continue;
            }
            // ShellEnv hooks must be synchronous — their stdout is the contract.
            let result = self.execute_sync(hook, &env_vars);
            if !result.success {
                tracing::warn!(
                    target: "hooks",
                    hook = result.name.as_deref().unwrap_or("(unnamed)"),
                    event = "shell_env",
                    exit_code = ?result.exit_code,
                    error = result.error.as_deref().unwrap_or(""),
                    "shell_env hook failed; contributing no env vars"
                );
                continue;
            }
            let parsed = parse_env_lines(&result.stdout);
            if parsed.is_empty() {
                continue;
            }
            // Audit-log the *keys* — never the values.
            crate::audit::log_sensitive_event(
                "shell_env_hook",
                serde_json::json!({
                    "hook": result.name,
                    "tool": context.tool_name,
                    "keys": parsed.keys().cloned().collect::<Vec<_>>(),
                }),
            );
            // Later hooks override earlier ones. Documented behavior.
            merged.extend(parsed);
        }
        merged
    }

    /// Execute all hooks for an event.
    ///
    /// The structured-stdin events (`turn_end`, `subagent_spawn`,
    /// `subagent_complete`) receive their JSON envelope on stdin; every
    /// other event runs with no stdin. Background hooks are the exception:
    /// they always run with null stdio (see [`Self::execute_background`])
    /// and never receive the envelope.
    pub fn execute(&self, event: HookEvent, context: &HookContext) -> Vec<HookResult> {
        if !self.config.enabled {
            return Vec::new();
        }

        let hooks = self.config.hooks_for_event(event);
        if hooks.is_empty() {
            // Fast path: no hooks for this event → skip the
            // `context.to_env_vars()` HashMap allocation. With
            // `tool_call_before` / `tool_call_after` firing per-tool
            // (#455) this allocation would otherwise happen on every
            // tool dispatch even for users with zero hooks configured.
            return Vec::new();
        }
        let env_vars = context.to_env_vars();
        let stdin_payload = match event {
            HookEvent::TurnEnd => Some(turn_end_payload(context)),
            HookEvent::SubagentSpawn | HookEvent::SubagentComplete => {
                Some(subagent_payload(event, context))
            }
            _ => None,
        };
        let mut results = Vec::new();

        for hook in hooks {
            if !self.matches_condition(hook, context) {
                continue;
            }

            let result = if hook.background {
                self.execute_background(hook, &env_vars)
            } else if let Some(payload) = stdin_payload.as_ref() {
                self.execute_sync_with_stdin(hook, &env_vars, payload)
            } else {
                self.execute_sync(hook, &env_vars)
            };

            // Log failures via tracing so operators tailing
            // `deepseek` with `RUST_LOG=warn` can see hook errors
            // without instrumenting each call site. Successful runs
            // log nothing (would be too noisy on per-tool events).
            if !result.success {
                let label = result.name.as_deref().unwrap_or("(unnamed)");
                tracing::warn!(
                    target: "hooks",
                    hook = label,
                    event = event.as_str(),
                    exit_code = ?result.exit_code,
                    duration_ms = result.duration.as_millis() as u64,
                    error = result.error.as_deref().unwrap_or(""),
                    stderr_head = %result.stderr.lines().next().unwrap_or(""),
                    "hook failed"
                );
            }

            let should_continue = result.success || hook.continue_on_error;
            results.push(result);

            if !should_continue {
                break;
            }
        }

        results
    }

    /// Check if a hook's condition matches the context
    fn matches_condition(&self, hook: &Hook, context: &HookContext) -> bool {
        match &hook.condition {
            None => true,
            Some(condition) => condition_matches(condition, context),
        }
    }

    /// Execute a hook synchronously
    fn execute_sync(&self, hook: &Hook, env_vars: &HashMap<String, String>) -> HookResult {
        self.execute_sync_inner(hook, env_vars, None)
    }

    /// Execute a hook synchronously with a structured JSON stdin payload.
    ///
    /// Used by mutable `message_submit` hooks and the structured-stdin
    /// observer events (`turn_end`, `subagent_spawn`, `subagent_complete`).
    /// Every other observer hook keeps the stdin-less [`Self::execute_sync`]
    /// path so its behavior is unchanged.
    fn execute_sync_with_stdin(
        &self,
        hook: &Hook,
        env_vars: &HashMap<String, String>,
        stdin_json: &serde_json::Value,
    ) -> HookResult {
        self.execute_sync_inner(hook, env_vars, Some(stdin_json))
    }

    fn execute_sync_inner(
        &self,
        hook: &Hook,
        env_vars: &HashMap<String, String>,
        stdin_json: Option<&serde_json::Value>,
    ) -> HookResult {
        let started = Instant::now();
        let working_dir = self
            .config
            .working_dir
            .clone()
            .unwrap_or_else(|| self.default_working_dir.clone());

        // Table-over-hook precedence (documented in `docs/HOOKS.md`): the
        // global `default_timeout_secs` override wins whenever it is set;
        // otherwise the hook's own `timeout_secs` (serde default 30)
        // applies.
        let timeout_secs = self
            .config
            .default_timeout_secs
            .unwrap_or(hook.timeout_secs);
        let timeout = Duration::from_secs(timeout_secs);

        let stdin_bytes = match stdin_json.map(serde_json::to_vec).transpose() {
            Ok(bytes) => bytes,
            Err(e) => {
                return HookResult {
                    name: hook.name.clone(),
                    success: false,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration: started.elapsed(),
                    error: Some(format!("Failed to encode hook stdin: {e}")),
                };
            }
        };

        let mut command = Self::build_shell_command(&hook.command);
        Self::own_process_group(&mut command);
        command
            .current_dir(&working_dir)
            .envs(env_vars)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if stdin_bytes.is_some() {
            command.stdin(Stdio::piped());
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                return HookResult {
                    name: hook.name.clone(),
                    success: false,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration: started.elapsed(),
                    error: Some(format!("Failed to spawn hook: {e}")),
                };
            }
        };

        let stdout_reader = child.stdout.take().map(spawn_pipe_reader);
        let stderr_reader = child.stderr.take().map(spawn_pipe_reader);
        let _stdin_writer = match (stdin_bytes, child.stdin.take()) {
            (Some(bytes), Some(stdin)) => Some(spawn_stdin_writer(stdin, bytes)),
            _ => None,
        };

        match child.wait_timeout(timeout) {
            Ok(Some(status)) => {
                // Bound the pipe-draining phase too: a descendant process
                // (nohup job &, daemonizer) inheriting the write end keeps
                // the pipe open after the direct child exits, and the
                // unbounded join below would then hang the caller forever —
                // the timeout arm avoids joining for exactly this reason.
                // The deadline covers both readers; whoever misses it
                // contributes empty output instead of a hang.
                let drain_deadline = std::time::Instant::now()
                    + std::time::Duration::from_secs(timeout_secs.clamp(1, 5));
                HookResult {
                    name: hook.name.clone(),
                    success: status.success(),
                    exit_code: status.code(),
                    stdout: join_reader_bounded(stdout_reader, drain_deadline),
                    stderr: join_reader_bounded(stderr_reader, drain_deadline),
                    duration: started.elapsed(),
                    error: None,
                }
            }
            Ok(None) => {
                kill_hook_child(&mut child);
                // Do not join pipe threads on timeout: descendant processes can
                // inherit pipe fds, and waiting for those threads would defeat
                // the hook timeout we just enforced.
                HookResult {
                    name: hook.name.clone(),
                    success: false,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration: started.elapsed(),
                    error: Some(format!("Hook timed out after {timeout_secs}s")),
                }
            }
            Err(e) => {
                kill_hook_child(&mut child);
                HookResult {
                    name: hook.name.clone(),
                    success: false,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration: started.elapsed(),
                    error: Some(format!("Failed to wait for hook: {e}")),
                }
            }
        }
    }

    /// Execute a hook in the background (non-blocking).
    ///
    /// Unlike the old fire-and-forget `.output()`, the child's lifetime is
    /// bounded by `default_timeout_secs` when set, else `hook.timeout_secs`
    /// (serde default 30) — the same resolution as the sync path: a hook
    /// that never exits is killed instead of leaking a process forever —
    /// on Unix the whole process group dies with it; on Windows only the
    /// direct child can be killed. All stdio is null — output is discarded
    /// (the previous form collected it and then threw it away), and the
    /// hook never receives the structured-stdin JSON envelope its
    /// foreground twin would have read.
    fn execute_background(&self, hook: &Hook, env_vars: &HashMap<String, String>) -> HookResult {
        let started = Instant::now();
        let working_dir = self
            .config
            .working_dir
            .clone()
            .unwrap_or_else(|| self.default_working_dir.clone());

        let cmd = hook.command.clone();
        let env = env_vars.clone();
        let wd = working_dir.clone();
        // Same resolution as the sync path (table-over-hook): the
        // executor-level `default_timeout_secs` override must bound
        // background hooks too — binding `hook.timeout_secs` directly
        // killed them at the serde default while foreground hooks ran to
        // the configured override.
        let timeout_secs = self
            .config
            .default_timeout_secs
            .unwrap_or(hook.timeout_secs);

        // Spawn in a detached thread
        std::thread::spawn(move || {
            let mut command = HookExecutor::build_shell_command(&cmd);
            HookExecutor::own_process_group(&mut command);
            let mut child = match command
                .current_dir(&wd)
                .envs(&env)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(child) => child,
                // Same diagnosability as the wait-error arm below: there
                // is no HookResult to carry the error (fire-and-forget),
                // so leave a trace — a hook that never spawned (missing
                // shell, deleted working dir, resource exhaustion) must
                // not fail silently.
                Err(err) => {
                    tracing::warn!(
                        target: "codesmith::hooks",
                        "background hook '{cmd}' failed to spawn: {err}"
                    );
                    return;
                }
            };
            let deadline = started + Duration::from_secs(timeout_secs.max(1));
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() >= deadline => {
                        kill_hook_child(&mut child);
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    // A wait error must not abandon the child un-reaped —
                    // same kill+reap as the timeout arm (the sync path's
                    // wait-error arm does this too). Unlike the sync path
                    // there is no HookResult to carry the error, so leave
                    // a trace — a hook dying this way must stay diagnosable.
                    Err(err) => {
                        tracing::warn!(
                            target: "codesmith::hooks",
                            "background hook '{cmd}' wait failed: {err}"
                        );
                        kill_hook_child(&mut child);
                        break;
                    }
                }
            }
        });

        // Return immediately with success (background execution is fire-and-forget)
        HookResult {
            name: hook.name.clone(),
            success: true,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            duration: started.elapsed(),
            error: None,
        }
    }
}

/// Recursive condition matcher, free-standing so `All`/`Any` recursion
/// borrows the nested `&HookCondition` directly — the previous form cloned
/// the whole `Hook` per sub-condition (multiplied per nesting level) on a
/// path that runs on every tool dispatch (#455).
fn condition_matches(condition: &HookCondition, context: &HookContext) -> bool {
    match condition {
        HookCondition::Always => true,
        HookCondition::ToolName { name } => context.tool_name.as_ref().is_some_and(|n| n == name),
        HookCondition::ToolCategory { category } => {
            // Map tool names to categories
            let tool_category = context.tool_name.as_ref().map(|name| match name.as_str() {
                "exec_shell" => "shell",
                "write_file" | "edit_file" | "apply_patch" => "file_write",
                "read_file" | "list_dir" | "grep_files" => "safe",
                _ => "other",
            });
            tool_category.is_some_and(|c| c == category.as_str())
        }
        HookCondition::Mode { mode } => context
            .mode
            .as_ref()
            .is_some_and(|m| m.to_lowercase() == mode.to_lowercase()),
        HookCondition::ExitCode { code } => context.tool_exit_code == Some(*code),
        HookCondition::All { conditions } => {
            conditions.iter().all(|c| condition_matches(c, context))
        }
        HookCondition::Any { conditions } => {
            conditions.iter().any(|c| condition_matches(c, context))
        }
    }
}

/// Kill a timed-out hook child. On Unix the hook runs in its own process
/// group (see [`HookExecutor::own_process_group`]), so the kill takes the
/// whole tree — killing only the `sh -c` wrapper leaves grandchildren
/// (backgrounded jobs, daemonizers) running, which the timeout exists to
/// prevent. On Windows only the direct child can be killed.
fn kill_hook_child(child: &mut std::process::Child) {
    #[cfg(not(windows))]
    {
        let _ = codesmith_agent_runtime::shell_manager::kill_child_process_group(child);
        let _ = child.wait();
    }
    #[cfg(windows)]
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn spawn_pipe_reader(mut pipe: impl Read + Send + 'static) -> JoinHandle<String> {
    thread::spawn(move || {
        // Read bytes, not a String: `read_to_string` fails at the first
        // invalid UTF-8 byte and would silently drop everything read so
        // far — one stray byte (localized output, binary noise) could
        // wipe a `pre_compact` preserve context or a `shell_env` block.
        // Decode lossily so the hook's whole contribution survives.
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(err) => {
                tracing::warn!(
                    target: "hooks",
                    "hook output was not valid UTF-8; invalid bytes were replaced"
                );
                String::from_utf8_lossy(err.as_bytes()).into_owned()
            }
        }
    })
}

/// Join a pipe reader, giving up at `deadline`. A stuck reader (a
/// descendant process holds the pipe open) leaks its thread — acceptable
/// next to hanging the whole hook pipeline; the process exits soon after.
fn join_reader_bounded(reader: Option<JoinHandle<String>>, deadline: std::time::Instant) -> String {
    use std::sync::mpsc;
    let Some(handle) = reader else {
        return String::new();
    };
    let (tx, rx) = mpsc::channel();
    let _forwarder = thread::spawn(move || {
        let _ = tx.send(handle.join().unwrap_or_default());
    });
    rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
        .unwrap_or_default()
    // `forwarder` may still be parked on the stuck reader — leaked by
    // design, see the doc above.
}

fn spawn_stdin_writer(mut stdin: std::process::ChildStdin, mut bytes: Vec<u8>) -> JoinHandle<()> {
    thread::spawn(move || {
        bytes.push(b'\n');
        let _ = stdin.write_all(&bytes);
        let _ = stdin.flush();
    })
}

fn message_submit_payload(context: &HookContext, text: &str) -> serde_json::Value {
    json!({
        "event": HookEvent::MessageSubmit.as_str(),
        "text": text,
        "session_id": context.session_id.as_deref(),
        "thread_id": context.thread_id.as_deref(),
        "workspace": context.workspace.as_ref().map(|path| path.display().to_string()),
        "mode": context.mode.as_deref(),
        "model": context.model.as_deref(),
        "total_tokens": context.total_tokens,
    })
}

/// Stdin envelope for `turn_end`. Observer-only — the hook's stdout is
/// ignored and `stop_hook_active` is always `false` today.
fn turn_end_payload(context: &HookContext) -> serde_json::Value {
    json!({
        "hook_event_name": HookEvent::TurnEnd.as_str(),
        "session_id": context.session_id.as_deref(),
        "thread_id": context.thread_id.as_deref(),
        "workspace": context.workspace.as_ref().map(|path| path.display().to_string()),
        "mode": context.mode.as_deref(),
        "model": context.model.as_deref(),
        "status": context.turn_status.as_deref(),
        "input_tokens": context.input_tokens,
        "output_tokens": context.output_tokens,
        "total_tokens": context.total_tokens,
        "session_cost": context.session_cost,
        "duration_secs": context.duration_secs,
        "stop_hook_active": false,
    })
}

/// Stdin envelope for `subagent_spawn` / `subagent_complete`. Observer-only.
/// The summary is bounded — truncated, never the full prompt/result.
fn subagent_payload(event: HookEvent, context: &HookContext) -> serde_json::Value {
    json!({
        "hook_event_name": event.as_str(),
        "session_id": context.session_id.as_deref(),
        "thread_id": context.thread_id.as_deref(),
        "workspace": context.workspace.as_ref().map(|path| path.display().to_string()),
        "mode": context.mode.as_deref(),
        "model": context.model.as_deref(),
        "agent_id": context.agent_id.as_deref(),
        "summary": context.summary.as_deref().map(truncate_hook_message),
    })
}

/// Fire an observer hook event without blocking the caller.
///
/// Used from the REPL and the sub-agent lifecycle, including sync call sites
/// (mode switches, spawn-under-manager-lock). When a Tokio runtime is
/// available the hook processes run on `spawn_blocking`, detached — the
/// caller does not wait; without a runtime (unit tests) they run inline.
/// Ordering-critical events (`session_start`, `session_end`,
/// `message_submit`) await the executor at their call sites instead.
pub(crate) fn emit_detached(host: Arc<dyn HookHost>, event: HookEvent, context: HookContext) {
    if !host.has_hooks_for_event(event) {
        return;
    }
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            // Drop the JoinHandle: the hook runs detached by design.
            std::mem::drop(handle.spawn_blocking(move || host.execute(event, &context)));
        }
        Err(_) => {
            host.execute(event, &context);
        }
    }
}

fn parse_message_submit_stdout(stdout: &str) -> MessageSubmitStdout {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return MessageSubmitStdout::Unchanged;
    }

    let value: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(value) => value,
        Err(e) => return MessageSubmitStdout::Invalid(format!("invalid JSON: {e}")),
    };

    let Some(object) = value.as_object() else {
        return MessageSubmitStdout::Invalid("stdout JSON must be an object".to_string());
    };

    match object.get("text") {
        Some(serde_json::Value::String(text)) if !text.is_empty() => {
            MessageSubmitStdout::Replaced(text.clone())
        }
        Some(serde_json::Value::String(_)) => {
            MessageSubmitStdout::Invalid("stdout `text` field must not be empty".to_string())
        }
        Some(_) => MessageSubmitStdout::Invalid("stdout `text` field must be a string".to_string()),
        None => MessageSubmitStdout::Unchanged,
    }
}

fn message_submit_continue_warning(result: &HookResult) -> Option<String> {
    message_submit_stdout_reason(&result.stdout)
        .or_else(|| first_non_empty_line(&result.stderr))
        .or_else(|| first_non_empty_line(&result.stdout))
        .or_else(|| result.error.as_deref().and_then(first_non_empty_line))
}

fn message_submit_block_reason(result: &HookResult, fallback: &str) -> String {
    if let Some(reason) = message_submit_stdout_reason(&result.stdout) {
        return reason;
    }
    if let Some(reason) = first_non_empty_line(&result.stderr) {
        return reason;
    }
    if let Some(reason) = first_non_empty_line(&result.stdout) {
        return reason;
    }
    if let Some(reason) = result.error.as_deref().and_then(first_non_empty_line) {
        return reason;
    }
    fallback.to_string()
}

fn message_submit_stdout_reason(stdout: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    value
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .map(truncate_hook_message)
}

fn first_non_empty_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(truncate_hook_message)
}

fn truncate_hook_message(message: &str) -> String {
    const MAX_CHARS: usize = 240;
    let mut chars = message.chars();
    let mut out: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

/// Parse `KEY=VALUE\n` lines from a `shell_env` hook's stdout into a map.
///
/// Tolerated: blank lines, leading whitespace, `#` comment lines (ignored),
/// `export KEY=VALUE` (the `export ` prefix is dropped), surrounding quotes
/// on the value. Lines without `=` are silently dropped — easier than
/// failing the whole hook for one stray line of human-friendly output.
/// Values are otherwise taken verbatim; we don't run them through a shell
/// for variable expansion to avoid surprises.
fn parse_env_lines(stdout: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for raw in stdout.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = value.trim();
        let stripped = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        out.insert(key.to_string(), stripped.to_string());
    }
    out
}

// === Unit Tests ===

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// #456 — `parse_env_lines` covers the formats users actually emit from
    /// shell hooks: bare `KEY=VAL`, `export KEY=VAL`, quoted values, comments,
    /// blank lines. Lines without `=` are dropped; values are taken verbatim
    /// (no shell expansion).
    #[test]
    fn parse_env_lines_handles_realistic_hook_output() {
        let stdout = r#"
# Aux comment line, ignored
AWS_ACCESS_KEY_ID=AKIAEXAMPLE
export GITHUB_TOKEN=ghp_examplevalue
QUOTED="value with spaces"
SINGLE='also valid'

= empty key dropped
NOEQUAL line dropped
"#;
        let parsed = super::parse_env_lines(stdout);
        assert_eq!(
            parsed.get("AWS_ACCESS_KEY_ID"),
            Some(&"AKIAEXAMPLE".to_string())
        );
        assert_eq!(
            parsed.get("GITHUB_TOKEN"),
            Some(&"ghp_examplevalue".to_string())
        );
        assert_eq!(parsed.get("QUOTED"), Some(&"value with spaces".to_string()));
        assert_eq!(parsed.get("SINGLE"), Some(&"also valid".to_string()));
        assert!(!parsed.contains_key(""));
        assert!(!parsed.contains_key("NOEQUAL line dropped"));
        // 4 valid entries above; nothing else.
        assert_eq!(parsed.len(), 4);
    }

    /// #456 — empty stdout (or only blank/comments) yields an empty map.
    #[test]
    fn parse_env_lines_empty_when_no_assignments() {
        let parsed = super::parse_env_lines("# nothing\n\n  \n");
        assert!(parsed.is_empty());
    }

    #[test]
    fn parse_message_submit_stdout_replaces_text() {
        assert_eq!(
            super::parse_message_submit_stdout(r#"{"text":"changed"}"#),
            MessageSubmitStdout::Replaced("changed".to_string())
        );
    }

    #[test]
    fn parse_message_submit_stdout_empty_is_unchanged() {
        assert_eq!(
            super::parse_message_submit_stdout(" \n\t "),
            MessageSubmitStdout::Unchanged
        );
    }

    #[test]
    fn parse_message_submit_stdout_without_text_is_unchanged() {
        assert_eq!(
            super::parse_message_submit_stdout(r#"{"reason":"only used for blocks"}"#),
            MessageSubmitStdout::Unchanged
        );
    }

    #[test]
    fn parse_message_submit_stdout_rejects_malformed_json() {
        assert!(matches!(
            super::parse_message_submit_stdout("not json"),
            MessageSubmitStdout::Invalid(_)
        ));
    }

    #[test]
    fn parse_message_submit_stdout_rejects_non_string_text() {
        assert!(matches!(
            super::parse_message_submit_stdout(r#"{"text":123}"#),
            MessageSubmitStdout::Invalid(_)
        ));
    }

    #[test]
    fn parse_message_submit_stdout_rejects_empty_text() {
        assert_eq!(
            super::parse_message_submit_stdout(r#"{"text":""}"#),
            MessageSubmitStdout::Invalid("stdout `text` field must not be empty".to_string())
        );
    }

    #[test]
    fn parse_message_submit_stdout_rejects_non_object_json() {
        assert!(matches!(
            super::parse_message_submit_stdout(r#"["not", "an", "object"]"#),
            MessageSubmitStdout::Invalid(_)
        ));
        assert!(matches!(
            super::parse_message_submit_stdout(r#""not an object""#),
            MessageSubmitStdout::Invalid(_)
        ));
    }

    #[test]
    fn test_hook_event_as_str() {
        assert_eq!(HookEvent::SessionStart.as_str(), "session_start");
        assert_eq!(HookEvent::ToolCallAfter.as_str(), "tool_call_after");
        assert_eq!(HookEvent::ModeChange.as_str(), "mode_change");
    }

    #[test]
    fn tool_hook_aliases_deserialize() {
        assert_eq!(
            serde_json::from_str::<HookEvent>(r#""pre_tool_use""#).unwrap(),
            HookEvent::ToolCallBefore
        );
        assert_eq!(
            serde_json::from_str::<HookEvent>(r#""post_tool_use""#).unwrap(),
            HookEvent::ToolCallAfter
        );
    }

    #[test]
    fn test_hook_context_to_env_vars() {
        let ctx = HookContext::new()
            .with_tool_name("exec_shell")
            .with_mode("agent")
            .with_workspace(PathBuf::from("/tmp"));

        let env = ctx.to_env_vars();

        assert_eq!(
            env.get("CODESMITH_TOOL_NAME"),
            Some(&"exec_shell".to_string())
        );
        assert_eq!(env.get("CODESMITH_MODE"), Some(&"agent".to_string()));
        assert_eq!(env.get("CODESMITH_WORKSPACE"), Some(&"/tmp".to_string()));
    }

    #[test]
    fn test_hook_condition_always() {
        let hook = Hook::new(HookEvent::SessionStart, "echo test");
        let executor = HookExecutor::disabled();
        let context = HookContext::new();

        assert!(executor.matches_condition(&hook, &context));
    }

    #[test]
    fn test_hook_condition_tool_name() {
        let hook = Hook::new(HookEvent::ToolCallBefore, "echo test").with_condition(
            HookCondition::ToolName {
                name: "exec_shell".to_string(),
            },
        );

        let executor = HookExecutor::disabled();

        let context_match = HookContext::new().with_tool_name("exec_shell");
        let context_no_match = HookContext::new().with_tool_name("write_file");

        assert!(executor.matches_condition(&hook, &context_match));
        assert!(!executor.matches_condition(&hook, &context_no_match));
    }

    #[test]
    fn test_hook_condition_mode() {
        let hook =
            Hook::new(HookEvent::ModeChange, "echo test").with_condition(HookCondition::Mode {
                mode: "agent".to_string(),
            });

        let executor = HookExecutor::disabled();

        let context_match = HookContext::new().with_mode("AGENT"); // Case insensitive
        let context_no_match = HookContext::new().with_mode("normal");

        assert!(executor.matches_condition(&hook, &context_match));
        assert!(!executor.matches_condition(&hook, &context_no_match));
    }

    #[test]
    fn test_hooks_config_for_event() {
        let config = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::SessionStart, "echo start"),
                Hook::new(HookEvent::SessionEnd, "echo end"),
                Hook::new(HookEvent::SessionStart, "echo start2"),
            ],
            ..Default::default()
        };

        let start_hooks = config.hooks_for_event(HookEvent::SessionStart);
        assert_eq!(start_hooks.len(), 2);

        let end_hooks = config.hooks_for_event(HookEvent::SessionEnd);
        assert_eq!(end_hooks.len(), 1);
    }

    #[test]
    fn test_hooks_config_disabled() {
        let config = HooksConfig {
            enabled: false,
            hooks: vec![Hook::new(HookEvent::SessionStart, "echo start")],
            ..Default::default()
        };

        let hooks = config.hooks_for_event(HookEvent::SessionStart);
        assert!(hooks.is_empty());
    }

    #[test]
    fn test_hook_builder() {
        let hook = Hook::new(HookEvent::ToolCallAfter, "notify.sh")
            .with_name("notify_tool")
            .with_timeout(60)
            .background()
            .with_condition(HookCondition::ToolCategory {
                category: "shell".to_string(),
            });

        assert_eq!(hook.name, Some("notify_tool".to_string()));
        assert_eq!(hook.timeout_secs, 60);
        assert!(hook.background);
        assert!(matches!(
            hook.condition,
            Some(HookCondition::ToolCategory { .. })
        ));
    }

    #[test]
    fn test_hook_timeout_enforced() {
        let command = if cfg!(windows) {
            "ping -n 3 127.0.0.1 > nul"
        } else {
            "sleep 2"
        };
        let hook = Hook::new(HookEvent::SessionStart, command).with_timeout(1);
        let executor = HookExecutor::new(HooksConfig::default(), PathBuf::from("."));
        let env_vars = HashMap::new();

        let result = executor.execute_sync(&hook, &env_vars);
        assert!(!result.success);
        assert!(
            result
                .error
                .as_ref()
                .is_some_and(|e| e.contains("timed out"))
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn message_submit_stdin_write_does_not_deadlock_when_hook_writes_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = write_hook_script(
            &dir,
            "write_before_read.sh",
            r#"#!/bin/sh
dd if=/dev/zero bs=1024 count=256 2>/dev/null | tr '\000' x
dd if=/dev/zero bs=1024 count=256 2>/dev/null | tr '\000' e >&2
payload=$(cat)
printf '\ndone:%s\n' "${#payload}"
"#,
        );
        let hook = Hook::new(HookEvent::MessageSubmit, &command).with_timeout(5);
        let executor = HookExecutor::new(HooksConfig::default(), dir.path().to_path_buf());
        let env_vars = HashMap::new();
        let payload = json!({
            "event": "message_submit",
            "text": "x".repeat(256 * 1024),
        });

        let result = executor.execute_sync_with_stdin(&hook, &env_vars, &payload);

        assert!(result.success, "hook should complete: {result:?}");
        assert!(result.stdout.contains("done:"), "stdout was drained");
        assert!(result.stderr.len() >= 256 * 1024, "stderr was drained");
    }

    #[test]
    fn test_executor_session_id() {
        let executor = HookExecutor::new(HooksConfig::default(), PathBuf::from("."));

        assert!(executor.session_id().starts_with("sess_"));
        assert_eq!(executor.session_id().len(), 13); // "sess_" + 8 chars
    }

    #[cfg(not(windows))]
    fn write_hook_script(dir: &tempfile::TempDir, name: &str, content: &str) -> String {
        let path = dir.path().join(name);
        std::fs::write(&path, content).expect("write hook script");
        format!("sh {}", path.display())
    }

    #[cfg(not(windows))]
    fn submit_context(dir: &tempfile::TempDir) -> HookContext {
        HookContext::new()
            .with_session_id("sess_test")
            .with_workspace(dir.path().to_path_buf())
            .with_mode("agent")
            .with_model("deepseek-test")
            .with_tokens(42)
    }

    #[cfg(not(windows))]
    #[test]
    fn message_submit_transform_applies_hooks_in_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = write_hook_script(
            &dir,
            "first.sh",
            r#"#!/bin/sh
printf '%s\n' '{"text":"first"}'
"#,
        );
        let second = write_hook_script(
            &dir,
            "second.sh",
            r#"#!/bin/sh
payload=$(cat)
case "$payload" in
  *'"text":"first"'*) printf '%s\n' '{"text":"first second"}' ;;
  *) printf '%s\n' '{"text":"wrong"}' ;;
esac
"#,
        );
        let config = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::MessageSubmit, &first),
                Hook::new(HookEvent::MessageSubmit, &second),
            ],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        assert_eq!(
            executor.execute_message_submit_transform(&submit_context(&dir), "original"),
            MessageSubmitOutcome::replaced("first second".to_string())
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn message_submit_transform_exit_two_blocks_submission() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = write_hook_script(
            &dir,
            "block.sh",
            r#"#!/bin/sh
printf '%s\n' '{"reason":"policy blocked this prompt"}'
exit 2
"#,
        );
        let config = HooksConfig {
            enabled: true,
            hooks: vec![Hook::new(HookEvent::MessageSubmit, &command)],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        assert_eq!(
            executor.execute_message_submit_transform(&submit_context(&dir), "original"),
            MessageSubmitOutcome::Blocked {
                reason: "policy blocked this prompt".to_string()
            }
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn background_message_submit_hook_is_observer_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = write_hook_script(
            &dir,
            "background.sh",
            r#"#!/bin/sh
printf '%s\n' '{"text":"ignored"}'
"#,
        );
        let config = HooksConfig {
            enabled: true,
            hooks: vec![Hook::new(HookEvent::MessageSubmit, &command).background()],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        assert_eq!(
            executor.execute_message_submit_transform(&submit_context(&dir), "original"),
            MessageSubmitOutcome::unchanged()
        );
    }

    #[test]
    fn message_submit_transform_without_configured_hooks_is_unchanged() {
        let executor = HookExecutor::new(HooksConfig::default(), PathBuf::from("."));

        assert_eq!(
            executor.execute_message_submit_transform(&HookContext::new(), "original"),
            MessageSubmitOutcome::unchanged()
        );
    }

    #[test]
    fn pre_compact_hook_without_configured_hooks_is_none() {
        let executor = HookExecutor::new(HooksConfig::default(), PathBuf::from("."));

        assert_eq!(executor.execute_pre_compact_hook(&HookContext::new()), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn pre_compact_hook_disabled_when_global_enabled_false() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = write_hook_script(&dir, "echo.sh", "printf 'remember the milk'");
        let config = HooksConfig {
            enabled: false,
            hooks: vec![Hook::new(HookEvent::PreCompact, &command)],
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        assert_eq!(
            executor.execute_pre_compact_hook(&submit_context(&dir)),
            None
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn pre_compact_hook_collects_stdout_as_preserve_context() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = write_hook_script(&dir, "preserve.sh", "printf 'remember the milk'");
        let config = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::PreCompact, &command)
                    .with_name("preserve")
                    .with_timeout(10),
            ],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        let result = executor.execute_pre_compact_hook(&submit_context(&dir));
        assert_eq!(result.as_deref(), Some("remember the milk"));
    }

    #[cfg(not(windows))]
    #[test]
    fn pre_compact_hook_concatenates_multiple_hooks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = write_hook_script(&dir, "first.sh", "printf 'fact A'");
        let second = write_hook_script(&dir, "second.sh", "printf 'fact B'");
        let config = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::PreCompact, &first),
                Hook::new(HookEvent::PreCompact, &second),
            ],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        let result = executor.execute_pre_compact_hook(&submit_context(&dir));
        let preserved = result.expect("at least one hook contributed");
        assert!(preserved.contains("fact A"));
        assert!(preserved.contains("fact B"));
        assert!(preserved.contains("---"));
    }

    #[cfg(not(windows))]
    #[test]
    fn pre_compact_hook_failed_hook_contributes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ok = write_hook_script(&dir, "ok.sh", "printf 'kept'");
        let bad = write_hook_script(&dir, "bad.sh", "printf 'lost'; exit 3");
        let config = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::PreCompact, &bad),
                Hook::new(HookEvent::PreCompact, &ok),
            ],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        // The failing hook's output is dropped; the successful one survives.
        let result = executor.execute_pre_compact_hook(&submit_context(&dir));
        let preserved = result.expect("successful hook contributed");
        assert!(preserved.contains("kept"));
        assert!(!preserved.contains("lost"));
    }

    #[cfg(not(windows))]
    #[test]
    fn pre_compact_hook_background_hook_is_observer_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = write_hook_script(&dir, "bg.sh", "printf 'ignored'");
        let config = HooksConfig {
            enabled: true,
            hooks: vec![Hook::new(HookEvent::PreCompact, &command).background()],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        assert_eq!(
            executor.execute_pre_compact_hook(&submit_context(&dir)),
            None
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn pre_compact_hook_receives_stdin_payload() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Echo a sentinel derived from the stdin JSON so we can assert the
        // structured payload actually reached the hook.
        let command = write_hook_script(
            &dir,
            "echo_payload.sh",
            r#"#!/bin/sh
payload=$(cat)
case "$payload" in
  *'"hook_event_name":"pre_compact"'*) printf 'saw pre_compact payload' ;;
  *) printf 'wrong payload' ;;
esac
"#,
        );
        let config = HooksConfig {
            enabled: true,
            hooks: vec![Hook::new(HookEvent::PreCompact, &command)],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        let result = executor.execute_pre_compact_hook(&submit_context(&dir));
        assert_eq!(result.as_deref(), Some("saw pre_compact payload"));
    }

    #[cfg(not(windows))]
    #[test]
    fn pre_compact_hook_skips_non_matching_condition() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = write_hook_script(&dir, "conditional.sh", "printf 'should not run'");
        let hook = Hook::new(HookEvent::PreCompact, &command).with_condition(HookCondition::Mode {
            mode: "plan".into(),
        });
        let config = HooksConfig {
            enabled: true,
            hooks: vec![hook],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        // submit_context sets mode = "agent", so a plan-only hook is skipped.
        assert_eq!(
            executor.execute_pre_compact_hook(&submit_context(&dir)),
            None
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn message_submit_transform_skips_non_matching_condition() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = write_hook_script(
            &dir,
            "replace.sh",
            r#"#!/bin/sh
printf '%s\n' '{"text":"should not apply"}'
"#,
        );
        let hook =
            Hook::new(HookEvent::MessageSubmit, &command).with_condition(HookCondition::Mode {
                mode: "plan".into(),
            });
        let config = HooksConfig {
            enabled: true,
            hooks: vec![hook],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        assert_eq!(
            executor.execute_message_submit_transform(&submit_context(&dir), "original"),
            MessageSubmitOutcome::unchanged()
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn message_submit_continue_on_error_true_keeps_text_and_runs_later_hooks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let failing = write_hook_script(
            &dir,
            "fail_continue.sh",
            r#"#!/bin/sh
printf '%s\n' 'soft failure' >&2
exit 9
"#,
        );
        let replacing = write_hook_script(
            &dir,
            "replace_after_failure.sh",
            r#"#!/bin/sh
printf '%s\n' '{"text":"recovered"}'
"#,
        );
        let config = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::MessageSubmit, &failing),
                Hook::new(HookEvent::MessageSubmit, &replacing),
            ],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        assert_eq!(
            executor.execute_message_submit_transform(&submit_context(&dir), "original"),
            MessageSubmitOutcome::replaced("recovered".to_string())
                .with_warning(Some("soft failure".to_string()))
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn message_submit_timeout_continue_surfaces_warning_and_runs_later_hooks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let slow = write_hook_script(
            &dir,
            "slow_continue.sh",
            r#"#!/bin/sh
sleep 2
"#,
        );
        let replacing = write_hook_script(
            &dir,
            "replace_after_timeout.sh",
            r#"#!/bin/sh
printf '%s\n' '{"text":"after timeout"}'
"#,
        );
        let mut slow_hook = Hook::new(HookEvent::MessageSubmit, &slow).with_timeout(1);
        slow_hook.continue_on_error = true;
        let config = HooksConfig {
            enabled: true,
            hooks: vec![slow_hook, Hook::new(HookEvent::MessageSubmit, &replacing)],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        assert_eq!(
            executor.execute_message_submit_transform(&submit_context(&dir), "original"),
            MessageSubmitOutcome::replaced("after timeout".to_string())
                .with_warning(Some("Hook timed out after 1s".to_string()))
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn message_submit_invalid_stdout_keeps_text_and_runs_later_hooks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let invalid = write_hook_script(
            &dir,
            "invalid_stdout.sh",
            r#"#!/bin/sh
printf '%s\n' 'not json'
"#,
        );
        let replacing = write_hook_script(
            &dir,
            "replace_after_invalid.sh",
            r#"#!/bin/sh
printf '%s\n' '{"text":"valid later"}'
"#,
        );
        let config = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::MessageSubmit, &invalid),
                Hook::new(HookEvent::MessageSubmit, &replacing),
            ],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        assert_eq!(
            executor.execute_message_submit_transform(&submit_context(&dir), "original"),
            MessageSubmitOutcome::replaced("valid later".to_string())
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn message_submit_continue_on_error_false_blocks_on_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = write_hook_script(
            &dir,
            "fail.sh",
            r#"#!/bin/sh
printf '%s\n' 'hard failure' >&2
exit 7
"#,
        );
        let mut hook = Hook::new(HookEvent::MessageSubmit, &command);
        hook.continue_on_error = false;
        let config = HooksConfig {
            enabled: true,
            hooks: vec![hook],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        assert_eq!(
            executor.execute_message_submit_transform(&submit_context(&dir), "original"),
            MessageSubmitOutcome::Blocked {
                reason: "hard failure".to_string()
            }
        );
    }

    #[test]
    fn has_hooks_for_event_fast_path_returns_false_for_empty_config() {
        let executor = HookExecutor::disabled();
        // No hooks configured AT ALL — every event is a fast skip.
        for event in [
            HookEvent::SessionStart,
            HookEvent::SessionEnd,
            HookEvent::MessageSubmit,
            HookEvent::ToolCallBefore,
            HookEvent::ToolCallAfter,
            HookEvent::ModeChange,
            HookEvent::OnError,
        ] {
            assert!(
                !executor.has_hooks_for_event(event),
                "empty config must short-circuit for {event:?}"
            );
        }
    }

    #[test]
    fn has_hooks_for_event_returns_false_when_globally_disabled() {
        let config = HooksConfig {
            enabled: false,
            hooks: vec![Hook::new(HookEvent::ToolCallBefore, "echo blocked")],
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, PathBuf::from("."));
        assert!(
            !executor.has_hooks_for_event(HookEvent::ToolCallBefore),
            "globally-disabled hooks must report no fires even when one is configured"
        );
    }

    #[test]
    fn has_hooks_for_event_distinguishes_event_types() {
        let config = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::SessionStart, "echo start"),
                Hook::new(HookEvent::ToolCallBefore, "echo before"),
            ],
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, PathBuf::from("."));
        // Configured events return true.
        assert!(executor.has_hooks_for_event(HookEvent::SessionStart));
        assert!(executor.has_hooks_for_event(HookEvent::ToolCallBefore));
        // Unconfigured events return false even when other events are present.
        assert!(!executor.has_hooks_for_event(HookEvent::ToolCallAfter));
        assert!(!executor.has_hooks_for_event(HookEvent::OnError));
        assert!(!executor.has_hooks_for_event(HookEvent::ModeChange));
    }

    #[test]
    fn subagent_payload_shape_and_summary_bound() {
        let long_summary = "x".repeat(500);
        let context = HookContext::new()
            .with_agent_id("agent_7")
            .with_summary(&long_summary)
            .with_model("deepseek-chat")
            .with_workspace(PathBuf::from("/tmp/ws"));
        let payload = subagent_payload(HookEvent::SubagentSpawn, &context);
        assert_eq!(payload["hook_event_name"], "subagent_spawn");
        assert_eq!(payload["agent_id"], "agent_7");
        assert_eq!(payload["model"], "deepseek-chat");
        assert_eq!(payload["workspace"], "/tmp/ws");
        // Bounded summary — never the full prompt/result.
        let summary = payload["summary"].as_str().expect("summary string");
        assert!(summary.chars().count() <= 241, "summary must be bounded");
        assert!(summary.ends_with('…'));
        // Fields the sub-agent tool context does not carry stay null.
        assert_eq!(payload["session_id"], serde_json::Value::Null);
        assert_eq!(payload["thread_id"], serde_json::Value::Null);
        assert_eq!(payload["mode"], serde_json::Value::Null);

        let complete = subagent_payload(HookEvent::SubagentComplete, &context);
        assert_eq!(complete["hook_event_name"], "subagent_complete");
        assert_eq!(complete["agent_id"], "agent_7");
    }

    #[cfg(not(windows))]
    #[test]
    fn turn_end_delivers_stdin_payload_to_hook() {
        let dir = tempfile::tempdir().expect("tempdir");
        let captured = dir.path().join("stdin.json");
        let capture_arg = captured.display().to_string();
        let command = write_hook_script(
            &dir,
            "capture_turn_end.sh",
            &format!(
                r#"#!/bin/sh
cat > '{capture_arg}'
"#
            ),
        );
        let config = HooksConfig {
            enabled: true,
            hooks: vec![Hook::new(HookEvent::TurnEnd, &command)],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        let context = HookContext::new()
            .with_session_id("sess_test1234")
            .with_thread_id("thread-abc")
            .with_workspace(dir.path().to_path_buf())
            .with_mode("agent")
            .with_model("deepseek-chat")
            .with_turn_status("completed")
            .with_input_tokens(120)
            .with_output_tokens(80)
            .with_tokens(1234)
            .with_duration_secs(14.5);
        let results = executor.execute(HookEvent::TurnEnd, &context);
        assert_eq!(results.len(), 1, "one matching hook must run");
        assert!(results[0].success, "hook must succeed");

        let payload: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&captured).expect("hook stdin captured"))
                .expect("stdin payload is valid JSON");
        assert_eq!(payload["hook_event_name"], "turn_end");
        assert_eq!(payload["session_id"], "sess_test1234");
        assert_eq!(payload["thread_id"], "thread-abc");
        assert_eq!(payload["mode"], "agent");
        assert_eq!(payload["status"], "completed");
        assert_eq!(payload["input_tokens"], 120);
        assert_eq!(payload["output_tokens"], 80);
        assert_eq!(payload["total_tokens"], 1234);
        assert_eq!(payload["duration_secs"], 14.5);
        assert_eq!(payload["stop_hook_active"], false);
    }
}
