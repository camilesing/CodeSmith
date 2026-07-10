//! Host-side [`AgentExecutor`] — the designated home for the production turn
//! loop migration (ROADMAP §E "接真引擎").
//!
//! The framework-core [`DefaultAgentExecutor`](codesmith_agent::executor::DefaultAgentExecutor)
//! is the minimal, host-agnostic reference loop. The production `Engine` in
//! this crate carries the real turn loop (`handle_deepseek_turn`, ~2.4k lines)
//! with ten guardrails (compaction / capacity / approval / steer /
//! transparent-retry / early-tool-start / subagent / LSP / loop-guard / cycle).
//! [`HostAgentExecutor`] is the host-side [`AgentExecutor`] impl that will
//! absorb those guardrails slice by slice, eventually replacing
//! `handle_deepseek_turn`. The three host→framework bridges are already in
//! place to compose it:
//!
//! - [`ToolSpecAdapter`](crate::tools::framework_adapter::ToolSpecAdapter) —
//!   production `ToolSpec`+`ToolContext` → framework `Tool` (the `run` path).
//! - [`CallbackBridge`](crate::callback_bridge::CallbackBridge) — `mpsc::Sender<Event>`
//!   + `HookHost` → framework `Callback` (tool-lifecycle hooks).
//! - [`SessionChatHistory`](crate::session_history::SessionChatHistory) —
//!   production `Session` → framework `ChatHistory` (the transcript).
//!
//! ## Absorbed guardrails
//!
//! [`HostAgentExecutor`] runs the LLM↔tool loop (reusing
//! [`accumulate_stream`](codesmith_agent::executor::accumulate_stream) for stream
//! reduction) and absorbs the production guardrails slice by slice. Two are in:
//!
//! 1. **loop-guard** ([`LoopGuard`]) — the 3rd identical tool call in a turn is
//!    blocked (a `ToolResult` error is fed back instead of executing), and 3 / 8
//!    consecutive failures of the same tool warn / halt the turn. The guard state
//!    is a local `LoopGuard` that persists across steps within one `run` (matching
//!    `turn_loop`). This was the proof that local-state guardrails need no
//!    interior mutability: `&self` suffices, `LoopGuard` is local, and
//!    `mpsc::Sender::send` takes `&self`.
//! 2. **LSP flush** ([`LspProbe`]) — the **first guardrail needing interior
//!    mutability**. After a successful edit (`edit_file` / `write_file`), the
//!    configured [`LspManagerApi`] is probed for diagnostics and the resulting
//!    [`DiagnosticBlock`]s accumulate in `LspProbe.pending` — an
//!    `Arc<std::sync::Mutex<Vec<DiagnosticBlock>>>`, because [`AgentExecutor::run`]
//!    is `&self` while the accumulator is mutated (push on collect, `mem::take` on
//!    flush). The lock is never held across an `await` (collect awaits
//!    `diagnostics_for` outside the lock; flush takes+drops the lock before
//!    `history.push`) — matching the [`CallbackBridge`](crate::callback_bridge::CallbackBridge)
//!    state pattern. Because the `Mutex` lives on the executor struct, pending
//!    diagnostics persist across `run` invocations on the same executor — matching
//!    the production `Engine.pending_lsp_blocks` field semantics (an edit on a turn
//!    that ends before the next request — e.g. a `MaxSteps` halt — surfaces its
//!    diagnostics on the next turn's first pre-request flush).
//!
//! Guardrail status (loop-guard warn/halt) surfaces over the host's `Event`
//! channel (`event_tx`) — **not** via the framework `Callback`: guardrails are
//! host-side concerns and the `Callback` trait stays untouched per ROADMAP §E.
//!
//! It is **not yet wired into `handle_send_message`**; the production
//! `handle_deepseek_turn` remains the live path — the value of landing it now is
//! the composition proof (the three bridges light up end-to-end inside a real
//! `AgentExecutor::run` driving a real `ToolSpec` over a real `Session`; see the
//! headline test) plus two guardrails absorbed at the seams below.
//!
//! ## Guardrail insertion points
//!
//! The loop has four seams where guardrails are absorbed incrementally:
//!
//! 1. **per-step pre-request** — ✅ **LSP flush** (drain pending diagnostics into
//!    a synthetic `user` message before the request snapshot); compaction,
//!    capacity pre-request, steer drain, system-prompt refresh still to come
//!    (top of the `loop`).
//! 2. **per-step post-stream** — transparent stream-retry, subagent handoff,
//!    thinking-only handling (after `accumulate_stream`, before tool extraction).
//!    *(not yet absorbed)*
//! 3. **per-tool** — ✅ **loop-guard `record_attempt`** (block the 3rd identical
//!    call) + **`record_outcome`** (warn at 3 / halt at 8 consecutive failures) +
//!    **LSP post-edit collect** (probe diagnostics after a successful edit);
//!    approval, early-tool-start, parallel dispatch still to come (inside the
//!    tool `for` loop).
//! 4. **per-step post-tool** — ✅ **loop-guard halt short-circuit** (returns
//!    `StopReason::Error`); capacity post-tool still to come (after the tool loop).
//!
//! Streaming deltas (`MessageDelta` / `ThinkingDelta`) will continue to flow
//! over the `Event` channel directly, emitted by an inline stream reducer (a
//! later slice replaces the `accumulate_stream` call) — they have no `Callback`
//! method and stay off the `Callback` path (see `callback_bridge` docs).
//!
//! ## Known gaps in the LSP flush (by design)
//!
//! - **`apply_patch` path derivation deferred** — production derives apply_patch
//!   edited paths via `HostServices::preflight_apply_patch_paths` (which calls
//!   `codesmith-tool-impls`, unreachable from this crate without a circular dep).
//!   This executor handles only `edit_file` / `write_file` (via the shared
//!   [`edit_file_paths`](super::lsp_hooks::edit_file_paths) helper); apply_patch
//!   collects nothing here. The live `handle_deepseek_turn` still covers it; this
//!   wires when the executor connects to a real `HostServices` (or a future
//!   resolver-closure injection).
//! - **no `<turn_meta>` enrichment** on the synthetic flush message — production
//!   wraps it in `user_text_message_with_turn_metadata` (date / model / working
//!   set / skills, read from `session` + `config`). The framework-executor path
//!   carries no turn metadata anywhere yet; that cross-cutting enrichment is its
//!   own future slice.
//! - **no `emit_session_updated`** for the synthetic push — the executor's other
//!   message pushes (assistant / tool result) likewise don't emit it via the
//!   `ChatHistory` path; UI surfacing is deferred to the wire-in step.
//!
//! See `ARCHITECTURE.md` ("Framework-core agent seam") and `ROADMAP.md` §E.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;

use codesmith_agent::callback::{Callback, StopReason};
use codesmith_agent::executor::{accumulate_stream, AgentExecutor, AgentExecutorConfig};
use codesmith_agent::llm_client::LlmClientHandle;
use codesmith_agent::memory::ChatHistory;
use codesmith_agent::models::{ContentBlock, Message, MessageRequest};
use codesmith_agent::tools::{ToolError, ToolResult, ToolSet};

use super::loop_guard::{AttemptDecision, LoopGuard, OutcomeDecision};
use super::lsp_hooks::edit_file_paths;
use crate::events::Event;
use crate::host_services::LspManagerApi;
use crate::lsp_diagnostics::{render_blocks as render_lsp_blocks, DiagnosticBlock};

/// The `ToolResult` fed back when the loop-guard blocks an identical repeat
/// call (mirrors `turn_loop::loop_guard_block_tool_result`). Duplicated here
/// rather than imported to keep this slice additive — zero production call-site
/// changes; a later cleanup can lift it into `loop_guard` proper as the single
/// source of truth.
fn block_tool_result(message: String) -> ToolResult {
    ToolResult::error(message).with_metadata(serde_json::json!({
        "loop_guard": "identical_tool_call"
    }))
}

/// Bundles the LSP collaborators the executor needs for the post-edit collect /
/// pre-request flush guardrail (§E, mirroring `Engine`'s
/// `run_post_edit_lsp_hook` / `flush_pending_lsp_diagnostics`).
///
/// Carries the **interior-mutable** diagnostics accumulator —
/// `Arc<Mutex<Vec<DiagnosticBlock>>>` — because [`AgentExecutor::run`] takes
/// `&self` while the accumulator is mutated (push on collect, `mem::take` on
/// flush). This mirrors the [`CallbackBridge`](crate::callback_bridge::CallbackBridge)
/// state pattern: a `std::sync::Mutex` whose guard is never held across an
/// `await` (collect awaits `diagnostics_for` *outside* the lock; flush takes
/// and drops the lock before pushing). Because the `Mutex` lives on the
/// executor struct (via this `Option<LspProbe>` field), pending diagnostics
/// persist across `run` invocations on the same executor — matching the
/// production `Engine.pending_lsp_blocks` field semantics (an edit on a turn
/// that ends before the next request — e.g. a `MaxSteps` halt — surfaces its
/// diagnostics on the next turn's first pre-request flush). `None` on the
/// executor ⇒ LSP disabled for this run (collect + flush are no-ops).
pub struct LspProbe {
    manager: Arc<dyn LspManagerApi>,
    /// Workspace root for relativizing edited paths (mirrors
    /// `session.workspace`, which `ChatHistory` does not expose).
    workspace: PathBuf,
    pending: Arc<std::sync::Mutex<Vec<DiagnosticBlock>>>,
}

impl LspProbe {
    /// Construct from the LSP manager + the session workspace. The pending
    /// accumulator starts empty (drained per-step on flush).
    #[must_use]
    pub fn new(manager: Arc<dyn LspManagerApi>, workspace: PathBuf) -> Self {
        Self {
            manager,
            workspace,
            pending: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

/// Host-side [`AgentExecutor`] — the growing home for the production turn loop.
///
/// Construct from the four framework collaborators: an [`LlmClientHandle`], a
/// [`ToolSet`] (built via
/// [`ToolRegistry::to_framework_tool_set`](crate::tools::registry::ToolRegistry::to_framework_tool_set)
/// in production), a [`Callback`] (a [`CallbackBridge`](crate::callback_bridge::CallbackBridge)
/// in production), and an [`AgentExecutorConfig`]. The optional `event_tx`
/// surfaces guardrail status (e.g. loop-guard warn/halt) onto the host's UI
/// `Event` channel — distinct from the `Callback`, which carries the framework
/// loop's own tool-lifecycle hooks. Nothing is mutated on `self` per run; the
/// transcript is mutated in place through [`ChatHistory`].
pub struct HostAgentExecutor {
    client: LlmClientHandle,
    tools: Arc<ToolSet>,
    callback: Arc<dyn Callback>,
    config: AgentExecutorConfig,
    event_tx: Option<mpsc::Sender<Event>>,
    /// Optional LSP diagnostics probe (§E). `None` ⇒ collect/flush no-op.
    lsp: Option<LspProbe>,
}

impl HostAgentExecutor {
/// Construct from the four collaborators + config + an optional guardrail
/// status channel (`None` for embeds that don't surface guardrail status) +
/// an optional [`LspProbe`] (`None` ⇒ LSP collect/flush disabled).
#[must_use]
pub fn new(
    client: LlmClientHandle,
    tools: Arc<ToolSet>,
    callback: Arc<dyn Callback>,
    config: AgentExecutorConfig,
    event_tx: Option<mpsc::Sender<Event>>,
    lsp: Option<LspProbe>,
) -> Self {
        Self {
            client,
            tools,
            callback,
            config,
            event_tx,
            lsp,
        }
    }

    /// Surface a guardrail status message onto the host's UI `Event` channel,
    /// if one was supplied. Guardrails emit here directly rather than through
    /// the framework `Callback` (see the module docs).
    async fn emit_status(&self, message: String) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(Event::status(message)).await;
        }
    }

    /// (3) per-tool post-edit seam — collect LSP diagnostics after a successful
    /// edit. Mirrors `Engine::run_post_edit_lsp_hook` (`lsp_hooks.rs`): gate on
    /// the master switch, derive the edited path, fetch diagnostics, push onto
    /// the interior-mutable accumulator. Failure is silent — a crashing LSP must
    /// never block the agent. `edit_file`/`write_file` paths come from the
    /// shared [`edit_file_paths`] helper; `apply_patch` path derivation is
    /// deferred (needs `HostServices::preflight_apply_patch_paths`, unreachable
    /// from this crate without the heavy host trait — see module docs).
    async fn collect_lsp_diagnostics(&self, tool_name: &str, input: &serde_json::Value) {
        let Some(probe) = &self.lsp else {
            return;
        };
        if !probe.manager.config().enabled {
            return;
        }
        let paths = match tool_name {
            "edit_file" | "write_file" => edit_file_paths(input),
            // apply_patch: deferred (needs HostServices); non-edit tools: nothing to probe.
            _ => Vec::new(),
        };
        for path in paths {
            let absolute = if path.is_absolute() {
                path
            } else {
                probe.workspace.join(&path)
            };
            // `edit_seq` is log-correlation only (production uses `turn_counter`);
            // this executor doesn't track a turn counter, so 0 suffices.
            if let Some(block) = probe.manager.diagnostics_for(&absolute, 0).await {
                probe.pending.lock().expect("poisoned").push(block);
            }
        }
    }

    /// (1) per-step pre-request seam — drain the pending LSP diagnostics into a
    /// synthetic `user` message so the model sees compile errors before its next
    /// reasoning step. Mirrors `Engine::flush_pending_lsp_diagnostics`
    /// (`lsp_hooks.rs`): `mem::take` the accumulator, render, push. No-op when
    /// nothing is pending or when LSP is disabled. Synchronous — the mutex guard
    /// is taken and dropped before `history.push`, never held across an `await`.
    fn flush_pending_lsp_diagnostics(&self, history: &mut dyn ChatHistory) {
        let Some(probe) = &self.lsp else {
            return;
        };
        let blocks = std::mem::take(&mut *probe.pending.lock().expect("poisoned"));
        if blocks.is_empty() {
            return;
        }
        let rendered = render_lsp_blocks(&blocks);
        if rendered.is_empty() {
            return;
        }
        // Plain `user` text message — no `<turn_meta>`: the framework-executor
        // path carries no turn metadata anywhere yet (`turn_metadata_block`
        // reads `session`+`config`, a cross-cutting host-side enrichment deferred
        // to its own slice). Pushed via `ChatHistory`, so it lands in the real
        // `Session` transcript ahead of the request snapshot below.
        history.push(Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: rendered,
                cache_control: None,
            }],
        });
    }
}

impl AgentExecutor for HostAgentExecutor {
    fn run<'a>(
        &'a self,
        history: &'a mut dyn ChatHistory,
        user_text: String,
    ) -> Pin<Box<dyn Future<Output = Result<StopReason>> + Send + 'a>> {
        Box::pin(self.run_inner(history, user_text))
    }
}

impl HostAgentExecutor {
    /// The bare LLM↔tool loop. Mirrors `DefaultAgentExecutor::run_inner`; will
    /// grow guardrail seams at the four points noted in the module docs.
    async fn run_inner<'a>(
        &'a self,
        history: &'a mut dyn ChatHistory,
        user_text: String,
    ) -> Result<StopReason> {
        // Cheap Arc clones so the loop body borrows locals, not `&self` fields.
        let client = self.client.clone();
        let tools = self.tools.clone();
        let callback = self.callback.clone();
        let max_steps = self.config.max_steps;
        let max_tokens = self.config.max_tokens;
        let system = self.config.system.clone();
        let temperature = self.config.temperature;

        // Seed the transcript with the user turn.
        history.push(Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: user_text,
                cache_control: None,
            }],
        });

        // Loop-guard state persists across steps within this run (one
        // `LoopGuard` per turn, matching `turn_loop`).
        let mut loop_guard = LoopGuard::default();
        let mut step: u32 = 0;
        loop {
            // (1) per-step pre-request seam — compaction / capacity / steer /
            // cycle land here later; ✅ LSP flush (drain pending diagnostics
            // into a synthetic user message before the request snapshot).
            if step >= max_steps {
                callback.on_complete(&StopReason::MaxSteps).await;
                return Ok(StopReason::MaxSteps);
            }
            // LSP flush sits after the max_steps bail so a turn-ending step
            // (e.g. MaxSteps right after an edit) leaves pending diagnostics
            // on the executor for the next turn's first flush — matching the
            // production `Engine.pending_lsp_blocks` field semantics.
            self.flush_pending_lsp_diagnostics(history);

            let api_tools = tools.to_api_tools();
            let request = MessageRequest {
                model: client.model().to_string(),
                messages: history.messages().to_vec(),
                max_tokens,
                system: system.clone(),
                tools: if api_tools.is_empty() {
                    None
                } else {
                    Some(api_tools)
                },
                tool_choice: None,
                metadata: None,
                thinking: None,
                reasoning_effort: None,
                stream: Some(true),
                temperature,
                top_p: None,
            };

            callback.on_llm_start(&request).await;
            let stream = client.create_message_stream(request).await?;
            let (content, _stop_reason) = accumulate_stream(stream).await?;
            // (2) per-step post-stream seam — transparent-retry / subagent land here.
            callback.on_llm_end(&content).await;

            // Persist the assistant turn.
            history.push(Message {
                role: "assistant".to_string(),
                content: content.clone(),
            });

            // Collect tool calls (preserve order).
            let tool_uses: Vec<(String, String, serde_json::Value)> = content
                .into_iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { id, name, input, .. } => Some((id, name, input)),
                    _ => None,
                })
                .collect();

            if tool_uses.is_empty() {
                callback.on_complete(&StopReason::NoToolCalls).await;
                return Ok(StopReason::NoToolCalls);
            }

            // Execute each tool sequentially and feed the result back as a
            // `role:"user"` `ToolResult` block (Anthropic/OpenAI-compat shape).
            //
            // (3) per-tool seam — loop-guard (absorbed); approval /
            // early-tool-start / parallel land here later. `loop_guard_halt` is
            // per-step: a halt short-circuits the tool loop and the whole turn
            // at the (4) seam below.
            let mut loop_guard_halt: Option<String> = None;
            for (id, name, input) in tool_uses {
                callback.on_tool_start(&name, &input).await;
                // loop-guard: block the 3rd identical (name+args) call this turn.
                let (result, blocked) = match loop_guard.record_attempt(&name, &input) {
                    AttemptDecision::Block(message) => {
                        (Ok(block_tool_result(message)), true)
                    }
                    AttemptDecision::Proceed => (
                        match tools.get(&name) {
                            Some(tool) => tool.run(input.clone()).await,
                            None => Err(ToolError::NotAvailable {
                                message: format!("no tool named '{name}'"),
                            }),
                        },
                        false,
                    ),
                };
                callback.on_tool_end(&name, &result).await;

                // loop-guard: track consecutive failures of this tool (warn at
                // 3, halt at 8). A guard-blocked call records no outcome — it
                // is an intervention, not an execution, so it doesn't count
                // toward the failure halt.
                if !blocked {
                    let success = result.as_ref().map(|r| r.success).unwrap_or(false);
                    match loop_guard.record_outcome(&name, success) {
                        OutcomeDecision::Continue => {}
                        OutcomeDecision::Warn(message) => {
                            tracing::warn!("{}", message);
                            self.emit_status(message).await;
                        }
                        OutcomeDecision::Halt(message) => {
                            loop_guard_halt.get_or_insert(message);
                        }
                    }
                }

                // (3) per-tool seam — loop-guard (absorbed); ✅ LSP post-edit
                // collect (only on a successful, non-blocked edit — mirrors
                // production `output.success && tool_was_executed`); approval /
                // early-tool-start / parallel land here later.
                if !blocked {
                    if let Ok(r) = &result {
                        if r.success {
                            self.collect_lsp_diagnostics(&name, &input).await;
                        }
                    }
                }

                let (content_str, is_error) = match &result {
                    Ok(r) => (r.content.clone(), !r.success),
                    Err(e) => (format!("Error: {e}"), true),
                };
                history.push(Message {
                    role: "user".to_string(),
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: id,
                        content: content_str,
                        is_error: Some(is_error),
                        content_blocks: None,
                    }],
                });
            }

            // (4) per-step post-tool seam — loop-guard halt (absorbed);
            // capacity / LSP post-edit land here later.
            if let Some(message) = loop_guard_halt {
                tracing::warn!("{}", message);
                self.emit_status(message.clone()).await;
                callback
                    .on_complete(&StopReason::Error(message.clone()))
                    .await;
                return Ok(StopReason::Error(message));
            }

            callback.on_step(step).await;
            step += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::callback_bridge::CallbackBridge;
    use crate::events::Event;
    use crate::hooks::{HookContext, HookEvent, HookHost, HookResult, MessageSubmitOutcome};
    use crate::host_services::LspManagerApi;
    use crate::lsp_config::LspConfig;
    use crate::lsp_diagnostics::{Diagnostic, DiagnosticBlock, Severity};
    use crate::session::Session;
    use crate::session_history::SessionChatHistory;
    use crate::tools::registry::ToolRegistry;
    use crate::tools::spec::{ToolContext, ToolSpec};
    use codesmith_agent::llm_client::{LlmClient, StreamEventBox};
    use codesmith_agent::models::{ContentBlockStart, Delta, MessageDelta, StreamEvent};
    use codesmith_agent::tools::{ToolCapability, ToolError, ToolResult};
    use std::collections::{HashMap, VecDeque};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    // === test doubles =======================================================

    /// A `ToolSpec` that echoes its `text` input, stamped with the captured
    /// workspace path so tests can prove the context flowed through the adapter.
    /// (Mirrors `framework_adapter` tests' `EchoSpec`.)
    struct EchoSpec;

    #[async_trait::async_trait]
    impl ToolSpec for EchoSpec {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes input, stamped with the workspace path."
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } }
            })
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            input: serde_json::Value,
            context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            let text = input
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(ToolResult {
                content: format!("{}|{text}", context.workspace.display()),
                success: true,
                metadata: None,
            })
        }
    }

    /// A `ToolSpec` standing in for `edit_file` / `write_file`: it succeeds and
    /// reports the edited `path` back in its content, so the §E LSP collect seam
    /// (keyed on tool name `edit_file`/`write_file` + the `path` input field)
    /// fires and the post-edit probe runs.
    struct EditSpec;

    #[async_trait::async_trait]
    impl ToolSpec for EditSpec {
        fn name(&self) -> &str {
            "edit_file"
        }
        fn description(&self) -> &str {
            "Edits a file at `path`; used to drive the LSP post-edit collect seam."
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } }
            })
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            input: serde_json::Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            let path = input
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(ToolResult {
                content: format!("edited:{path}"),
                success: true,
                metadata: None,
            })
        }
    }

    /// Like `EditSpec` (name `edit_file`, reads `path`) but reports a *failed*
    /// edit (`success: false`) so tests can prove the LSP collect seam is gated
    /// on a successful edit (mirrors production `output.success`).
    struct FailingEditSpec;

    #[async_trait::async_trait]
    impl ToolSpec for FailingEditSpec {
        fn name(&self) -> &str {
            "edit_file"
        }
        fn description(&self) -> &str {
            "An edit that fails; the LSP collect seam must skip it."
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } }
            })
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            input: serde_json::Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            let path = input
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(ToolResult {
                content: format!("edit failed:{path}"),
                success: false,
                metadata: None,
            })
        }
    }

    /// A `LlmClient` that pops canned `StreamEvent` lists from a queue, one
    /// per `create_message_stream` call. (Mirrors the bridge tests' `MockLlm`.)
    /// Also records the `messages` of every received request, so tests can
    /// prove the model saw a specific synthetic message (e.g. flushed LSP
    /// diagnostics) before a given call.
    struct MockLlm {
        calls: Mutex<VecDeque<Vec<StreamEvent>>>,
        requests: Mutex<Vec<Vec<Message>>>,
    }

    impl MockLlm {
        fn new(calls: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                calls: Mutex::new(calls.into_iter().collect()),
                requests: Mutex::new(Vec::new()),
            }
        }

        /// The `messages` snapshot of each `create_message_stream` call, in call
        /// order.
        fn requests(&self) -> Vec<Vec<Message>> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl LlmClient for MockLlm {
        fn provider_name(&self) -> &'static str {
            "mock"
        }
        fn model(&self) -> &str {
            "mock-v0"
        }
        fn create_message(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<codesmith_agent::models::MessageResponse>> + Send + '_>>
        {
            Box::pin(async { anyhow::bail!("mock does not implement create_message") })
        }
        fn create_message_stream(
            &self,
            request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<StreamEventBox>> + Send + '_>> {
            self.requests.lock().unwrap().push(request.messages.clone());
            let next = self.calls.lock().unwrap().pop_front();
            Box::pin(async move {
                let events = next.unwrap_or_default();
                Ok(Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)))
                    as StreamEventBox)
            })
        }
    }

    /// A `LspManagerApi` test double. Owns an `LspConfig` (lent via `config()`)
    /// and returns a canned `DiagnosticBlock` per `diagnostics_for` call, while
    /// recording every (file, edit_seq) it was probed with. `enabled(false)`
    /// short-circuits the collect seam at the master switch before any probe.
    struct FakeLsp {
        config: LspConfig,
        diagnostics: Option<DiagnosticBlock>,
        calls: Mutex<Vec<(PathBuf, u64)>>,
    }

    impl FakeLsp {
        /// Enabled LSP that returns `block` for every probed file.
        fn returning(block: DiagnosticBlock) -> Arc<Self> {
            Arc::new(Self {
                config: LspConfig {
                    enabled: true,
                    ..LspConfig::default()
                },
                diagnostics: Some(block),
                calls: Mutex::new(Vec::new()),
            })
        }

        /// Disabled LSP — `config().enabled == false`, so the collect seam
        /// early-returns before probing.
        fn disabled() -> Arc<Self> {
            Arc::new(Self {
                config: LspConfig {
                    enabled: false,
                    ..LspConfig::default()
                },
                diagnostics: None,
                calls: Mutex::new(Vec::new()),
            })
        }

        /// The `(file, edit_seq)` pairs `diagnostics_for` was probed with.
        fn calls(&self) -> Vec<(PathBuf, u64)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl LspManagerApi for FakeLsp {
        fn config(&self) -> &LspConfig {
            &self.config
        }
        async fn diagnostics_for(&self, file: &Path, edit_seq: u64) -> Option<DiagnosticBlock> {
            self.calls
                .lock()
                .unwrap()
                .push((file.to_path_buf(), edit_seq));
            self.diagnostics.clone()
        }
    }

    /// A `HookHost` test double that records every `execute` call.
    /// (Mirrors `callback_bridge` tests' `RecordingHookHost`.)
    #[derive(Default)]
    struct RecordingHookHost {
        calls: std::sync::Arc<Mutex<Vec<(HookEvent, HookContext)>>>,
    }

    impl RecordingHookHost {
        fn calls(&self) -> Vec<(HookEvent, HookContext)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl HookHost for RecordingHookHost {
        fn execute(&self, event: HookEvent, context: &HookContext) -> Vec<HookResult> {
            self.calls
                .lock()
                .unwrap()
                .push((event, context.clone()));
            Vec::new()
        }
        fn execute_pre_compact_hook(&self, _context: &HookContext) -> Option<String> {
            None
        }
        fn execute_message_submit_transform(
            &self,
            _context: &HookContext,
            _original_text: &str,
        ) -> MessageSubmitOutcome {
            MessageSubmitOutcome::unchanged()
        }
        fn has_hooks_for_event(&self, _event: HookEvent) -> bool {
            true
        }
        fn is_enabled(&self) -> bool {
            true
        }
        fn session_id(&self) -> &str {
            "test"
        }
        fn collect_shell_env(&self, _context: &HookContext) -> HashMap<String, String> {
            HashMap::new()
        }
    }

    fn test_template() -> HookContext {
        HookContext::new()
            .with_session_id("test")
            .with_workspace(PathBuf::from("/tmp/codesmith-test"))
            .with_model("mock-v0")
    }

    // === stream-event builders (mirroring the bridge tests) =================

    fn text_block(idx: u32, body: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ContentBlockStart {
                index: idx,
                content_block: ContentBlockStart::Text {
                    text: String::new(),
                },
            },
            StreamEvent::ContentBlockDelta {
                index: idx,
                delta: Delta::TextDelta {
                    text: body.to_string(),
                },
            },
            StreamEvent::ContentBlockStop { index: idx },
        ]
    }

    fn tool_use_block(idx: u32, id: &str, name: &str, input_json: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ContentBlockStart {
                index: idx,
                content_block: ContentBlockStart::ToolUse {
                    id: id.to_string(),
                    name: name.to_string(),
                    input: serde_json::Value::Null,
                    caller: None,
                },
            },
            StreamEvent::ContentBlockDelta {
                index: idx,
                delta: Delta::InputJsonDelta {
                    partial_json: input_json.to_string(),
                },
            },
            StreamEvent::ContentBlockStop { index: idx },
        ]
    }

    fn finish(stop: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::MessageDelta {
                delta: MessageDelta {
                    stop_reason: Some(stop.to_string()),
                    stop_sequence: None,
                },
                usage: None,
            },
            StreamEvent::MessageStop,
        ]
    }

    /// One-file `DiagnosticBlock` with a single ERROR line, the canned payload
    /// `FakeLsp::returning` hands back per probe.
    fn error_diag_block(file: &str, line: u32, column: u32, message: &str) -> DiagnosticBlock {
        DiagnosticBlock {
            file: PathBuf::from(file),
            items: vec![Diagnostic {
                line,
                column,
                severity: Severity::Error,
                message: message.to_string(),
            }],
        }
    }

    /// Drain all events currently buffered in `rx` into a `Vec`.
    fn drain(rx: &mut mpsc::Receiver<Event>) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            out.push(event);
        }
        out
    }

    fn fresh_session() -> Session {
        Session::new(
            "mock-v0".to_string(),
            PathBuf::from("/tmp/codesmith-test"),
            false,
            false,
            PathBuf::from("/tmp/codesmith-test/notes.md"),
            PathBuf::from("/tmp/codesmith-test/mcp.json"),
        )
    }

    // === tests ==============================================================

    #[tokio::test]
    async fn host_executor_drives_full_bridge_trio() {
        // Registry with a real ToolSpec → framework ToolSet via the adapter.
        let tmp = tempdir().expect("tempdir");
        let workspace_stamp = tmp.path().display().to_string();
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EchoSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        // Real Session → framework ChatHistory.
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);

        // CallbackBridge: mock Event channel + RecordingHookHost.
        let (tx, mut rx) = mpsc::channel(256);
        let hooks = Arc::new(RecordingHookHost::default());
        let callback: Arc<dyn Callback> = Arc::new(CallbackBridge::new(
            Some(tx),
            Some(hooks.clone()),
            test_template(),
        ));

        // Call 1: text + tool_use(echo). Call 2: text-only -> NoToolCalls.
        let mut call1 = text_block(0, "let me echo");
        call1.extend(tool_use_block(1, "t1", "echo", r#"{"text":"world"}"#));
        call1.extend(finish("tool_use"));
        let mut call2 = text_block(0, "done");
        call2.extend(finish("end_turn"));

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![call1, call2])),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "echo world".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // [user, assistant(text+tooluse), user(toolresult), assistant(text)]
        assert_eq!(history.len(), 4);
        // The same bytes live on the underlying Session.
        assert_eq!(sess.messages.len(), 4);

        // The ToolResult carries the captured context's workspace path —
        // proof the ToolSpec flowed through ToolSpecAdapter into the loop.
        match &sess.messages[2].content[0] {
            ContentBlock::ToolResult {
                content,
                is_error,
                ..
            } => {
                assert!(
                    content.starts_with(&workspace_stamp),
                    "context stamped: {content}"
                );
                assert!(content.ends_with("|world"));
                assert_eq!(*is_error, Some(false));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }

        // Event channel: ToolCallStarted + ToolCallComplete with matching ids.
        let events = drain(&mut rx);
        let started = events
            .iter()
            .find_map(|e| match e {
                Event::ToolCallStarted { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
            .expect("ToolCallStarted emitted");
        let complete = events
            .iter()
            .find_map(|e| match e {
                Event::ToolCallComplete { id, name, result } => {
                    Some((id.clone(), name.clone(), result.clone()))
                }
                _ => None,
            })
            .expect("ToolCallComplete emitted");
        assert_eq!(started.0, complete.0, "bridge ids correlate");
        assert_eq!(started.1, "echo");
        assert_eq!(started.2, serde_json::json!({"text":"world"}));
        assert_eq!(complete.1, "echo");
        match complete.2 {
            Ok(r) => {
                assert!(r.content.ends_with("|world"));
                assert!(r.success);
            }
            Err(e) => panic!("expected Ok ToolResult, got Err: {e}"),
        }

        // HookHost: ToolCallBefore + ToolCallAfter with full context.
        let calls = hooks.calls();
        assert_eq!(calls.len(), 2, "one Before + one After");
        assert_eq!(calls[0].0, HookEvent::ToolCallBefore);
        assert_eq!(calls[1].0, HookEvent::ToolCallAfter);
        assert_eq!(calls[0].1.tool_name.as_deref(), Some("echo"));
        assert_eq!(calls[0].1.session_id.as_deref(), Some("test"));
        assert_eq!(calls[1].1.tool_name.as_deref(), Some("echo"));
        assert_eq!(calls[1].1.tool_result.as_deref().unwrap().ends_with("|world"), true);
        assert_eq!(calls[1].1.tool_success, Some(true));
    }

    #[tokio::test]
    async fn host_executor_missing_tool_records_error_result() {
        // Empty ToolSet -> "ghost" lookup fails with NotAvailable.
        let tools = Arc::new(ToolSet::new());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let mut call = text_block(0, "calling ghost");
        call.extend(tool_use_block(1, "t1", "ghost", r#"{}"#));
        call.extend(finish("tool_use"));
        let mut finish_call = text_block(0, "ok");
        finish_call.extend(finish("end_turn"));

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![call, finish_call])),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        match &sess.messages[2].content[0] {
            ContentBlock::ToolResult {
                content,
                is_error,
                ..
            } => {
                assert!(content.starts_with("Error:"));
                assert_eq!(*is_error, Some(true));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn host_executor_exhausts_steps() {
        // Mock always returns a tool call -> hits MaxSteps.
        let tmp = tempdir().expect("tempdir");
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EchoSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let make_call = || {
            let mut c = text_block(0, "looping");
            c.extend(tool_use_block(1, "t1", "echo", r#"{"text":"x"}"#));
            c.extend(finish("tool_use"));
            c
        };

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![make_call(), make_call(), make_call()])),
            tools,
            callback,
            AgentExecutorConfig {
                max_steps: 2,
                ..AgentExecutorConfig::default()
            },
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::MaxSteps);
        // user + (assistant + toolresult) x2 = 1 + 2*2 = 5
        assert_eq!(history.len(), 5);
    }

    // === loop-guard (seam 3 + 4) ===========================================

    #[tokio::test]
    async fn loop_guard_blocks_third_identical_call() {
        let tmp = tempdir().expect("tempdir");
        let workspace_stamp = tmp.path().display().to_string();
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EchoSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        // Three identical echo calls, then a text-only turn that ends the run.
        let call = || {
            let mut c = text_block(0, "again");
            c.extend(tool_use_block(1, "t1", "echo", r#"{"text":"x"}"#));
            c.extend(finish("tool_use"));
            c
        };
        let mut done = text_block(0, "done");
        done.extend(finish("end_turn"));

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![call(), call(), call(), done])),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // [user, asst, tr(echo), asst, tr(echo), asst, tr(block), asst] = 8.
        assert_eq!(history.len(), 8);

        // First two tool results are real echo output (workspace-stamped) —
        // proof the tool actually ran twice.
        for &idx in &[2usize, 4] {
            match &sess.messages[idx].content[0] {
                ContentBlock::ToolResult { content, is_error, .. } => {
                    assert!(
                        content.starts_with(&workspace_stamp),
                        "echo ran at msg[{idx}]: {content}"
                    );
                    assert_eq!(*is_error, Some(false));
                }
                other => panic!("msg[{idx}] not ToolResult: {other:?}"),
            }
        }
        // Third is the loop-guard block — echo did NOT run, error, block message.
        match &sess.messages[6].content[0] {
            ContentBlock::ToolResult { content, is_error, .. } => {
                assert!(
                    !content.starts_with(&workspace_stamp),
                    "echo must not run on the blocked call: {content}"
                );
                assert!(
                    content.contains("already been made 3 times"),
                    "block message: {content}"
                );
                assert_eq!(*is_error, Some(true));
            }
            other => panic!("msg[6] not ToolResult: {other:?}"),
        }
    }

    #[tokio::test]
    async fn loop_guard_warns_at_three_failures() {
        // No tools registered — every tool call hits "ghost" (NotAvailable),
        // which counts as a failure for the loop-guard.
        let tools = Arc::new(ToolSet::new());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        // Vary the args each call so `record_attempt` (keyed on name+args) never
        // blocks; `record_outcome` is keyed on name only, so failures still
        // accumulate toward the warn threshold (3).
        let failing = |n: u64| {
            let mut c = text_block(0, "trying");
            c.extend(tool_use_block(1, "t1", "ghost", &format!(r#"{{"n":{n}}}"#)));
            c.extend(finish("tool_use"));
            c
        };
        let mut done = text_block(0, "done");
        done.extend(finish("end_turn"));

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![failing(1), failing(2), failing(3), done])),
            tools,
            callback,
            AgentExecutorConfig::default(),
            Some(tx),
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        let events = drain(&mut rx);
        let warned = events.iter().any(|e| {
            matches!(
                e,
                Event::Status { message } if message.contains("failed 3 consecutive times")
            )
        });
        assert!(warned, "expected a warn status event, got: {events:?}");
    }

    #[tokio::test]
    async fn loop_guard_halts_after_eight_failures() {
        let tools = Arc::new(ToolSet::new()); // ghost

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let failing = |n: u64| {
            let mut c = text_block(0, "trying");
            c.extend(tool_use_block(1, "t1", "ghost", &format!(r#"{{"n":{n}}}"#)));
            c.extend(finish("tool_use"));
            c
        };
        // 8 distinct-arg failures → the 8th triggers Halt.
        let calls: Vec<Vec<StreamEvent>> = (1..=8).map(failing).collect();

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(calls)),
            tools,
            callback,
            AgentExecutorConfig::default(),
            Some(tx),
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        let msg = match reason {
            StopReason::Error(m) => m,
            other => panic!("expected Error, got {other:?}"),
        };
        assert!(
            msg.contains("failed 8 consecutive times"),
            "halt message: {msg}"
        );

        let events = drain(&mut rx);
        let halted = events.iter().any(|e| {
            matches!(
                e,
                Event::Status { message } if message.contains("failed 8 consecutive times")
            )
        });
        assert!(halted, "expected a halt status event, got: {events:?}");
    }

    // === LSP flush (seam 1 + 3) ==========================================

    /// Helper: does any message in `sess` carry a `<diagnostics` text block?
    fn has_diagnostics_msg(sess: &Session) -> bool {
        sess.messages.iter().any(|m| {
            m.content.iter().any(|b| {
                matches!(b, ContentBlock::Text { text, .. } if text.contains("<diagnostics"))
            })
        })
    }

    #[tokio::test]
    async fn lsp_collect_then_flush_feeds_model() {
        let tmp = tempdir().expect("tempdir");
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EditSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let fake = FakeLsp::returning(error_diag_block("foo.rs", 12, 8, "missing semicolon"));
        let probe = LspProbe::new(fake.clone(), tmp.path().to_path_buf());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        // Call 1: edit_file -> tool_use (collect probes LSP). Call 2: text -> end.
        let mut call1 = text_block(0, "editing");
        call1.extend(tool_use_block(1, "t1", "edit_file", r#"{"path":"foo.rs"}"#));
        call1.extend(finish("tool_use"));
        let mut call2 = text_block(0, "done");
        call2.extend(finish("end_turn"));

        let mock = Arc::new(MockLlm::new(vec![call1, call2]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            Some(probe),
        );

        let reason = executor
            .run(&mut history, "edit foo.rs".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // [user, asst(text+tooluse), user(toolresult), user(<diagnostics>), asst]
        assert_eq!(history.len(), 5);
        assert_eq!(sess.messages[3].role.as_str(), "user");
        match &sess.messages[3].content[0] {
            ContentBlock::Text { text, .. } => {
                assert!(text.contains("<diagnostics"), "rendered block: {text}");
                assert!(text.contains("missing semicolon"));
                assert!(text.contains("foo.rs"));
            }
            other => panic!("expected diagnostics Text block, got {other:?}"),
        }

        // The model actually saw it — call2's request snapshot included it.
        let reqs = mock.requests();
        assert_eq!(reqs.len(), 2);
        let saw_diag = reqs[1].iter().any(|m| {
            m.content.iter().any(|b| {
                matches!(b, ContentBlock::Text { text, .. } if text.contains("<diagnostics"))
            })
        });
        assert!(saw_diag, "call2 request must include diagnostics: {reqs:?}");

        // Probed once, for the edited file (relativized to the workspace).
        assert_eq!(fake.calls().len(), 1);
        assert!(
            fake.calls()[0].0.ends_with("foo.rs"),
            "probed path: {:?}",
            fake.calls()[0].0
        );
    }

    #[tokio::test]
    async fn lsp_disabled_skips_collect() {
        // Unit check of the master-switch gate inside `collect_lsp_diagnostics`.
        let fake = FakeLsp::disabled();
        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![])),
            Arc::new(ToolSet::new()),
            Arc::new(codesmith_agent::callback::NoopCallback),
            AgentExecutorConfig::default(),
            None,
            Some(LspProbe::new(fake.clone(), PathBuf::from("/tmp/ws"))),
        );
        executor
            .collect_lsp_diagnostics("edit_file", &serde_json::json!({"path":"foo.rs"}))
            .await;
        assert!(
            fake.calls().is_empty(),
            "disabled LSP must not be probed: {:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn lsp_skips_non_edit_tool() {
        // Non-edit tool name → no path derivation → no probe.
        let fake = FakeLsp::returning(error_diag_block("foo.rs", 1, 1, "x"));
        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![])),
            Arc::new(ToolSet::new()),
            Arc::new(codesmith_agent::callback::NoopCallback),
            AgentExecutorConfig::default(),
            None,
            Some(LspProbe::new(fake.clone(), PathBuf::from("/tmp/ws"))),
        );
        executor
            .collect_lsp_diagnostics("echo", &serde_json::json!({"text":"hi"}))
            .await;
        assert!(
            fake.calls().is_empty(),
            "non-edit tool must not probe LSP: {:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn lsp_skips_failed_edit() {
        // The loop's success gate (r.success) must skip collect on a failed edit.
        let tmp = tempdir().expect("tempdir");
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(FailingEditSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let fake = FakeLsp::returning(error_diag_block("foo.rs", 1, 1, "stale"));
        let probe = LspProbe::new(fake.clone(), tmp.path().to_path_buf());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let mut call1 = text_block(0, "editing");
        call1.extend(tool_use_block(1, "t1", "edit_file", r#"{"path":"foo.rs"}"#));
        call1.extend(finish("tool_use"));
        let mut call2 = text_block(0, "done");
        call2.extend(finish("end_turn"));

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![call1, call2])),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            Some(probe),
        );

        let reason = executor
            .run(&mut history, "edit foo.rs".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        assert!(
            fake.calls().is_empty(),
            "failed edit must not probe LSP: {:?}",
            fake.calls()
        );
        assert!(!has_diagnostics_msg(&sess), "no diagnostics message expected");
    }

    #[tokio::test]
    async fn lsp_apply_patch_paths_deferred() {
        // apply_patch path derivation is deferred (needs HostServices) — collect
        // must not probe even though config is enabled. Pins the gap; flips when
        // the executor later wires a real HostServices.
        let fake = FakeLsp::returning(error_diag_block("a.rs", 1, 1, "x"));
        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![])),
            Arc::new(ToolSet::new()),
            Arc::new(codesmith_agent::callback::NoopCallback),
            AgentExecutorConfig::default(),
            None,
            Some(LspProbe::new(fake.clone(), PathBuf::from("/tmp/ws"))),
        );
        executor
            .collect_lsp_diagnostics("apply_patch", &serde_json::json!({"patch":"x"}))
            .await;
        assert!(
            fake.calls().is_empty(),
            "apply_patch must not probe LSP yet: {:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn lsp_cross_turn_persistence_via_shared_state() {
        // THE interior-mutability proof: `pending_lsp_blocks` (Arc<Mutex<Vec>>)
        // persists across `run()` calls on the SAME executor. run1 edits then hits
        // MaxSteps (max_steps:1) before flushing, leaving pending non-empty; run2
        // on a fresh session flushes those leftovers into its first request.
        let tmp = tempdir().expect("tempdir");
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EditSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let fake = FakeLsp::returning(error_diag_block("foo.rs", 12, 8, "missing semicolon"));
        let probe = LspProbe::new(fake.clone(), tmp.path().to_path_buf());

        let mock = Arc::new(MockLlm::new(vec![
            // run1: edit -> tool_use (then MaxSteps halts before a 2nd request).
            {
                let mut c = text_block(0, "editing");
                c.extend(tool_use_block(1, "t1", "edit_file", r#"{"path":"foo.rs"}"#));
                c.extend(finish("tool_use"));
                c
            },
            // run2: text-only -> end (NoToolCalls).
            {
                let mut c = text_block(0, "ok");
                c.extend(finish("end_turn"));
                c
            },
        ]));

        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            Arc::new(codesmith_agent::callback::NoopCallback),
            AgentExecutorConfig {
                max_steps: 1,
                ..AgentExecutorConfig::default()
            },
            None,
            Some(probe),
        );

        // Run 1: edits foo.rs (collect pushes to pending), then MaxSteps halts
        // before the next step's flush (the max_steps bail precedes the flush
        // seam), so pending carries over.
        let mut sess_a = fresh_session();
        let mut history_a = SessionChatHistory::new(&mut sess_a);
        let reason = executor
            .run(&mut history_a, "edit foo.rs".to_string())
            .await
            .expect("run1");
        assert_eq!(reason, StopReason::MaxSteps);
        assert!(!has_diagnostics_msg(&sess_a), "run1 must not flush before MaxSteps");

        // Run 2: SAME executor, FRESH session. The first pre-request flush must
        // drain run1's leftover pending into run2's transcript — impossible with
        // a per-run local Vec; proves the Arc<Mutex<Vec>> persists across runs.
        let mut sess_b = fresh_session();
        let mut history_b = SessionChatHistory::new(&mut sess_b);
        let reason = executor
            .run(&mut history_b, "next turn".to_string())
            .await
            .expect("run2");
        assert_eq!(reason, StopReason::NoToolCalls);

        // sess_b: [user_text, <diagnostics flush>, asst] — from run1's edit.
        assert_eq!(history_b.len(), 3);
        assert_eq!(sess_b.messages[1].role.as_str(), "user");
        match &sess_b.messages[1].content[0] {
            ContentBlock::Text { text, .. } => {
                assert!(text.contains("<diagnostics"), "flush block: {text}");
                assert!(text.contains("missing semicolon"));
            }
            other => panic!("expected diagnostics flush msg, got {other:?}"),
        }
        // And the model saw it in run2's (only) request.
        let reqs = mock.requests();
        assert_eq!(reqs.len(), 2, "run1 + run2 each fired one request");
        let saw = reqs[1].iter().any(|m| {
            m.content.iter().any(|b| {
                matches!(b, ContentBlock::Text { text, .. } if text.contains("<diagnostics"))
            })
        });
        assert!(saw, "run2 request must include leftover diagnostics: {reqs:?}");
    }
}
