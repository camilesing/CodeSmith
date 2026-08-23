//! Turn-loop stream phase: inline stream reduction, transparent retry, and
//! early speculative tool dispatch (early-tool-start).
//!
//! Split out of `engine/host_executor.rs` (codebase-health §1, step 1).
//! [`HostAgentExecutor::reduce_stream`] consumes a `StreamEventBox` while
//! forwarding deltas to the [`Callback`](codesmith_agent::callback::Callback)
//! in real time; [`HostAgentExecutor::stream_with_transparent_retry`] drives
//! the request / transparent-retry / reactive-recovery round (cancel
//! Checkpoints B/C/D); the early-tool-start machinery ([`EarlyToolTask`] +
//! [`early_start_safe`]) speculatively runs read-only tools at
//! `ContentBlockStop` so their results are ready at execute time. The full
//! turn-loop narrative (guardrail inventory, seam numbering, the step loop)
//! stays in the `host_executor.rs` module docs; the two gap inventories below
//! moved here together with the code they describe.

//! ## Known gaps in transparent-retry (by design)
//!
//! - **bail-on-error gap closed** ✅ — the inline `reduce_stream` reducer (§E
//!   inline-stream-reduction slice) replaced the CORE `accumulate_stream` call.
//!   It tracks `any_content_received` and returns `StreamReduceOutcome::Partial`
//!   (surface partial content, don't retry) when the stream dies after content
//!   was produced, and `StreamReduceOutcome::Empty` (retry transparently) only
//!   when no content arrived. This mirrors production's
//!   `should_transparently_retry_stream` guard (`streaming.rs:81-87`). The old
//!   `accumulate_stream` dropped partial blocks on the first erroring item, so
//!   the executor retried even when production would ship partial content — that
//!   gap is now closed.
//! - **pre-stream connection errors: reactive recovery absorbed** ✅ — a
//!   `create_message_stream` `Err` is now classified via
//!   `is_context_length_error_message` before propagating. A context-length
//!   rejection triggers `try_recover_context_overflow` (emergency compaction);
//!   on success the step restarts so the request snapshot picks up the
//!   compacted transcript, and on failure (or a non-context-length error) the
//!   turn hard-fails (mirrors `handle_deepseek_turn`). Only mid-flight stream
//!   errors (from `reduce_stream`) retry transparently; pre-stream errors are
//!   either recovered or hard-failed.
//! - **cancel-token short-circuit** ✅ — production's
//!   `should_transparently_retry_stream` checks `!cancelled` to abort a retry
//!   loop on a cancelled turn. This executor now mirrors it: Checkpoint C (the
//!   `Empty` arm) checks `is_cancelled()` and returns
//!   `StreamRoundOutcome::Interrupted` (no retry); Checkpoint B (stream-open
//!   race) and Checkpoint D (post-stream `Complete`/`Partial`) are also wired.
//!   See guardrail 10 (cancel-token) for the full checkpoint map.

//! ## Known gaps in early-tool-start (by design)
//!
//! - **`ToolCallStarted` emitted at stream time ✅** — [`reduce_stream`] fires
//!   `StreamDelta::ToolCallStarted` on `ContentBlockStop` for tool blocks
//!   (carrying the wire id), so the UI shows "calling X" before the tool
//!   executes. The `Callback::on_tool_start` trait now carries the wire `id`,
//!   so the [`CallbackBridge`](crate::callback_bridge::CallbackBridge) uses the real wire id (no more `bridge-{n}`
//!   synthesis) and deduplicates: the stream-time `ToolCallStarted` marks the
//!   id as announced, and the execute-time `on_tool_start` skips re-emitting.
//! - **static-only safety gate** — [`early_start_safe`] checks
//!   [`Tool::capabilities`](codesmith_agent::tools::Tool::capabilities) (`ReadOnly` present AND none of
//!   `{RequiresApproval, ExecutesCode, WritesFiles}`), mirroring the final
//!   composite clause of `handle_deepseek_turn`'s `early_tool_start_safe`. Production
//!   additionally checks `metadata.is_read_only &&
//!   metadata.supports_parallel && !is_interactive && validate_input().is_ok()
//!   && approval_requirement_for(...) == Auto` plus a tool-catalog allowlist
//!   (not-MCP / not-code-execution / not-tool-search). Those per-input /
//!   per-metadata surfaces are not reachable from the framework `Tool` trait;
//!   they thread in at the wire-in step (§E design note, same gap as
//!   [`requires_approval`]). The practical effect: a read-only tool whose
//!   per-input validation would reject the args is spawned speculatively and
//!   the (rejected) result is discarded at execute time — wasted work, not a
//!   correctness bug.
//! - **no loop-guard at spawn time** — production's `early_tool_start_safe`
//!   consults the `LoopGuard` so a 3rd-identical call isn't speculatively
//!   started (it'd be blocked at execute time anyway). This executor spawns
//!   without consulting `LoopGuard` to avoid threading a `&mut LoopGuard`
//!   through the streaming path (which would double-count the attempt — once at
//!   spawn, once at execute). The execute-time `record_attempt` is the single
//!   source of truth: a speculatively-started task for a call that the
//!   loop-guard blocks is popped + `Drop`-aborted in the tool loop (the
//!   `AttemptDecision::Block` arm). So the wasted work is a spawned task that's
//!   immediately aborted — cheap for a read-only tool.
//! - **no per-input approval / interactive checks** — `early_start_safe`
//!   statically excludes `RequiresApproval`-tagged tools, but a tool that's
//!   `Auto`-by-default yet `Required` for a specific input (e.g. `exec_shell rm`)
//!   isn't visible from `capabilities()`. Such a tool would be speculatively
//!   started and, if the execute-time approval comes back `Required`, the task
//!   is popped + `Drop`-aborted (the approval `Err(denial)` arm). Again wasted
//!   work, not a correctness bug. Threads in with the per-input approval
//!   surface at the wire-in step.
//! - **cancel-token short-circuit** — by design (N/A): production's
//!   `early_tool_start_safe` doesn't check cancel at spawn time either; a
//!   cancelled turn still spawns speculative tasks for tool blocks that close
//!   before the stream errors, but the `early_tasks.clear()` at the end of the
//!   step (and `Drop` on map drop) aborts them. The work is bounded by the
//!   number of tool blocks in the partial stream. Checkpoint A (loop-top) and
//!   Checkpoint D (post-stream) bound the turn before the next step's
//!   speculative dispatch.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::Result;
use futures_util::StreamExt;

use codesmith_agent::callback::StreamDelta;
use codesmith_agent::llm_client::{LlmClientHandle, StreamEventBox};
use codesmith_agent::memory::ChatHistory;
use codesmith_agent::models::{
    ContentBlock, ContentBlockStart, Delta, MessageDelta, MessageRequest, StreamEvent,
    SystemPrompt, ToolCaller, Usage,
};
use codesmith_agent::tools::{ToolCapability, ToolError, ToolResult};

use crate::engine::context::{
    MAX_CONTEXT_RECOVERY_ATTEMPTS, context_input_budget_for_provider,
    is_context_length_error_message,
};
use crate::engine::host_executor::{HostAgentExecutor, requires_approval};
use crate::engine::summarize_text;

/// Whether a tool is a safe candidate for **early speculative dispatch**
/// (early-tool-start, §E): the read-only, parallel-safe, no-approval, no-side-
/// effect tools whose results can be pre-computed during streaming and reused
/// at execute time (mirrors `handle_deepseek_turn`'s `early_tool_start_safe` final composite
/// gate). The framework `Tool` trait exposes only `capabilities()`, so this is a
/// **static approximation**: `ReadOnly` present AND none of `{RequiresApproval,
/// ExecutesCode, WritesFiles}`. Production additionally checks
/// `metadata.is_read_only && metadata.supports_parallel && !is_interactive &&
/// validate_input().is_ok() && approval_requirement_for(...) == Auto` plus a
/// tool-catalog allowlist (not-MCP / not-code-execution / not-tool-search) —
/// those per-input / per-metadata surfaces are not reachable from the framework
/// `Tool` and thread in at the wire-in step (§E design note, same gap as
/// [`requires_approval`]). `Network` / `Sandboxable` capabilities are not
/// disqualifying (a read-only network fetch is safe to start early).
pub(crate) fn early_start_safe(caps: &[ToolCapability]) -> bool {
    let read_only = caps.contains(&ToolCapability::ReadOnly);
    read_only && !requires_approval(caps)
}

/// Accumulator for a single content block being built from streaming deltas.
/// Mirrors `accumulate_stream`'s local `BlockBuild` (in
/// `codesmith-agent::executor`) — kept here as a private duplicate so the
/// inline reducer can emit deltas *while* accumulating, which the CORE reducer
/// cannot (it has no `Callback` handle). The `BTreeMap<u32, BlockBuild>` keying
/// preserves wire order (block indices are monotonic but not necessarily
/// contiguous, so `BTreeMap` not `Vec`).
enum BlockBuild {
    Text(String),
    Thinking(String),
    ToolUse {
        id: String,
        name: String,
        input_buf: String,
        start_input: serde_json::Value,
        caller: Option<ToolCaller>,
    },
}

/// Resolve a tool block's final input from its accumulated `input_buf`
/// (streamed `InputJsonDelta` fragments) with a fallback to the
/// `start_input` carried by `ContentBlockStart::ToolUse`, and finally to an
/// empty object — the same logic as the CORE `accumulate_stream`'s tail.
/// Extracted so both [`finalize_blocks`] (at stream end) and the early-start
/// spawn (at `ContentBlockStop`, mid-stream) finalize identically.
fn finalize_tool_input(input_buf: &str, start_input: &serde_json::Value) -> serde_json::Value {
    if !input_buf.is_empty() {
        serde_json::from_str(input_buf).unwrap_or(serde_json::Value::Null)
    } else if !start_input.is_null() {
        start_input.clone()
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    }
}

/// A speculatively-dispatched read-only tool task started during streaming
/// (early-tool-start, §E). When a `ContentBlockStop` for a tool block lands
/// mid-stream, [`reduce_stream`](HostAgentExecutor::reduce_stream) checks
/// [`early_start_safe`] and, if the tool qualifies, spawns it on the runtime
/// so its result is ready by the time the executor reaches the tool loop. At
/// execute time the executor pops the task by wire `id`, re-verifies the
/// name + input (the model could in principle revise args after the block
/// closed), and awaits the `JoinHandle` to reuse the result instead of
/// re-running the tool (mirrors `handle_deepseek_turn`'s `early_tool_tasks` map +
/// `EarlyToolTask`).
///
/// `Drop` aborts the spawned task so an unreused / orphaned task (e.g. a
/// block that didn't survive into the parsed `tool_uses`, or a call blocked
/// by the loop-guard / denied approval at execute time) never leaks a
/// background task. Aborting a task that already completed is a no-op, so the
/// reuse path (await then drop) is safe. The handle is wrapped in `Option` so
/// the reuse path can [`Option::take`] it out for `.await` — a type that
/// implements `Drop` can't otherwise let a field be moved out of it.
pub(crate) struct EarlyToolTask {
    /// Tool name (re-verified at execute time).
    pub(crate) name: String,
    /// Finalized input (re-verified at execute time).
    pub(crate) input: serde_json::Value,
    /// The speculative task. `Some` until the reuse path [`Option::take`]s
    /// it for `.await`; aborted on every other path (via `Drop`).
    pub(crate) handle: Option<tokio::task::JoinHandle<Result<ToolResult, ToolError>>>,
}

impl Drop for EarlyToolTask {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            // `abort` takes `&self`; safe on a completed task (no-op).
            handle.abort();
        }
    }
}

/// Finalize an accumulated `BlockBuild` map into assembled `ContentBlock`s.
/// This is the same assembly logic as `accumulate_stream`'s tail — extracted
/// so the inline reducer can call it both on clean completion and on
/// mid-flight error (partial content).
fn finalize_blocks(blocks: BTreeMap<u32, BlockBuild>) -> Vec<ContentBlock> {
    blocks
        .into_values()
        .map(|build| match build {
            BlockBuild::Text(text) => ContentBlock::Text {
                text,
                cache_control: None,
            },
            BlockBuild::Thinking(thinking) => ContentBlock::Thinking { thinking },
            BlockBuild::ToolUse {
                id,
                name,
                input_buf,
                start_input,
                caller,
            } => {
                let input = finalize_tool_input(&input_buf, &start_input);
                ContentBlock::ToolUse {
                    id,
                    name,
                    input,
                    caller,
                }
            }
        })
        .collect()
}

/// Outcome of the inline stream reducer ([`HostAgentExecutor::reduce_stream`]).
/// Replaces the CORE `accumulate_stream`'s binary `Result<(Vec<ContentBlock>,
/// Option<String>)>` with a three-way result that distinguishes "clean
/// completion" from "partial content + error" from "empty + error" — the
/// distinction drives the transparent-retry decision (only `Empty` retries).
enum StreamReduceOutcome {
    /// Stream completed cleanly (either `MessageStop` seen or the stream ended
    /// without error). The assembled content blocks and stop reason are
    /// available.
    Complete {
        content: Vec<ContentBlock>,
        stop_reason: Option<String>,
        /// Token usage captured for this stream (slice 21 §E):
        /// `MessageStart` sets input tokens, `MessageDelta` overwrites with
        /// the latest cumulative usage (replace-within-stream — mirrors the
        /// retired `handle_deepseek_turn`). The caller adds it to the per-turn
        /// total (mirrors `handle_deepseek_turn`'s `turn.add_usage`).
        usage: Usage,
    },
    /// The stream produced content (text/thinking/tool deltas arrived) and
    /// then died mid-flight. The partial content assembled so far is available
    /// — the caller should surface it (not retry), matching production's
    /// `any_content_received` guard (`handle_deepseek_turn`: once the user has
    /// seen output, retrying double-bills and loses the partial turn).
    Partial {
        content: Vec<ContentBlock>,
        stop_reason: Option<String>,
        error: String,
        /// Same capture as [`StreamReduceOutcome::Complete`]'s `usage` — the
        /// partial stream's usage up to the mid-flight death (surfaced, not
        /// retried, so its tokens still count).
        usage: Usage,
    },
    /// The stream died before any content was produced (only `MessageStart`
    /// or nothing arrived). Safe to retry transparently — the provider hasn't
    /// billed for output and the user has seen nothing (mirrors
    /// `should_transparently_retry_stream` in `streaming.rs:81`).
    Empty { error: String },
}

/// Outcome of one (possibly retried) stream round in
/// [`HostAgentExecutor::stream_with_transparent_retry`]. Replaces the binary
/// `Result<(Vec<ContentBlock>, Option<String>)>` so the caller can distinguish
/// "content produced — proceed" from "reactive recovery succeeded — restart
/// the step" (the seam-2 reactive context-length recovery signal).
pub(crate) enum StreamRoundOutcome {
    /// The stream produced content (clean completion or partial surfacing).
    /// The assembled blocks and stop reason feed back into the transcript.
    Content {
        content: Vec<ContentBlock>,
        stop_reason: Option<String>,
        /// Per-stream usage threaded up from [`reduce_stream`] (slice 21 §E)
        /// — `run_inner` adds it to the per-turn total via
        /// [`HostAgentExecutor::accumulate_usage`].
        usage: Usage,
    },
    /// A pre-stream context-length rejection was classified (via
    /// [`is_context_length_error_message`]) and emergency compaction succeeded
    /// — the caller should `continue` the step loop so the request snapshot
    /// picks up the compacted transcript (mirrors `handle_deepseek_turn`).
    RecoveredContextOverflow,
    /// The turn was cancelled during the stream phase (Checkpoint B race at
    /// stream-open, Checkpoint C guard in the transparent-retry `Empty` arm,
    /// or Checkpoint D post-stream gate). The caller should surface
    /// [`StopReason::Interrupted`](codesmith_agent::callback::StopReason::Interrupted) — mirroring production's
    /// `TurnOutcomeStatus::Interrupted` (`handle_deepseek_turn`).
    Interrupted,
}

impl HostAgentExecutor {
    /// Inline stream reducer — replaces the CORE `accumulate_stream` call so
    /// the executor can emit streaming deltas to the [`Callback`](codesmith_agent::callback::Callback) in real time
    /// and track `any_content_received` (closing the transparent-retry
    /// bail-on-error gap). This is the §E inline-stream-reduction slice.
    ///
    /// The accumulation logic mirrors `accumulate_stream`
    /// (`codesmith-agent::executor::mod.rs`): a `BTreeMap<u32, BlockBuild>`
    /// keyed by the wire content-block index, with text/thinking deltas
    /// appended to their block's buffer and tool-input JSON deltas buffered
    /// for a final `serde_json::from_str` at assembly time. The key difference
    /// from the CORE reducer is that each text/thinking delta is **also**
    /// forwarded to [`Callback::on_stream_delta`](codesmith_agent::callback::Callback::on_stream_delta) before being buffered, so
    /// the host's UI lights up as the stream arrives (not after the whole
    /// stream is buffered).
    ///
    /// `any_content_received` flips on the first non-`MessageStart` event —
    /// the moment we cross from "stream not yet productive" (eligible for
    /// transparent retry) into "the model has billed for output" (must
    /// surface). On a mid-flight `Err`, this drives the
    /// [`StreamReduceOutcome`] variant: `Empty` (no content → safe to retry)
    /// vs `Partial` (content received → surface, don't retry). This mirrors
    /// production's `any_content_received` guard in
    /// `should_transparently_retry_stream` (`streaming.rs:81-87`).
    ///
    /// Tool-input JSON deltas (`Delta::InputJsonDelta`) are **not** emitted
    /// to the callback — they're assembled into the `ToolUse` block's input,
    /// which isn't user-visible until `on_llm_end`. Block-lifecycle events
    /// (`MessageStarted` / `ThinkingStarted` / `ThinkingComplete` /
    /// `MessageComplete`) **are** synthesized here, at `ContentBlockStart` /
    /// `ContentBlockStop` for text/thinking blocks — letting the host's UI frame
    /// a block before its first delta and mark it done when its last delta
    /// lands (matching production's `handle_deepseek_turn`).
    ///
    /// **Early speculative dispatch (early-tool-start):** when a tool block
    /// reaches `ContentBlockStop`, its input is finalized and — if the tool is
    /// [`early_start_safe`] — a [`tokio::spawn`] runs it immediately so its
    /// result is ready by the time the executor reaches the tool loop. The
    /// `JoinHandle` is stored in `early_tasks` keyed by the wire tool id; the
    /// tool loop pops it by id, re-verifies name + input, and awaits it to
    /// reuse the result (mirrors `handle_deepseek_turn`'s `early_tool_tasks` map +
    /// `early_tool_start_safe`). `Event::ToolCallStarted` is also fired on
    /// `ContentBlockStop` for tool blocks via `StreamDelta::ToolCallStarted`
    /// (carrying the wire id) — the `CallbackBridge` deduplicates this
    /// stream-time emission against the execute-time `on_tool_start`.
    async fn reduce_stream(
        &self,
        mut stream: StreamEventBox,
        early_tasks: &mut HashMap<String, EarlyToolTask>,
        pending_steers: &mut Vec<String>,
    ) -> StreamReduceOutcome {
        let mut blocks: BTreeMap<u32, BlockBuild> = BTreeMap::new();
        let mut stop_reason: Option<String> = None;
        let mut any_content_received = false;
        // Per-stream usage (slice 21 §E): `MessageStart` sets input tokens,
        // `MessageDelta` overwrites with the latest cumulative usage
        // (replace-within-stream — mirrors the retired `handle_deepseek_turn`).
        // Returned on `Complete`/`Partial` so the caller adds it to the
        // per-turn total (mirrors `handle_deepseek_turn`). `Empty`
        // carries none — a retried stream's partial usage is dropped, matching
        // production's `continue` before `turn.add_usage`.
        let mut usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            ..Usage::default()
        };

        while let Some(item) = stream.next().await {
            let event = match item {
                Ok(e) => e,
                Err(e) => {
                    // Stream died mid-flight. Whether to retry depends on
                    // whether any content was produced before the error.
                    let error = e.to_string();
                    if any_content_received {
                        let content = finalize_blocks(std::mem::take(&mut blocks));
                        return StreamReduceOutcome::Partial {
                            content,
                            stop_reason,
                            error,
                            usage,
                        };
                    }
                    return StreamReduceOutcome::Empty { error };
                }
            };

            // Flip on the first non-MessageStart event — that's the moment we
            // cross from "stream not yet productive" into "the model has billed
            // for output" (mirrors `handle_deepseek_turn`).
            if !any_content_received && !matches!(event, StreamEvent::MessageStart { .. }) {
                any_content_received = true;
            }

            // Mid-stream steer buffer (mirrors `handle_deepseek_turn`): drain
            // any steers that arrived while the stream was producing. These are
            // buffered (not injected mid-stream) and flushed by `run_inner`
            // post-stream / post-tool — without this, steers arriving during the
            // last step's streaming would be discarded by the next turn's stale
            // drain. The tokio mutex guard is taken and dropped within the `{ }`
            // block before `emit_status().await` — no lock crosses an `await`
            // (matching `drain_steers`).
            if let Some(rx) = &self.steer {
                loop {
                    let steer = {
                        let mut guard = rx.lock().await;
                        match guard.try_recv() {
                            Ok(s) => s,
                            Err(_) => break,
                        }
                    };
                    let steer = steer.trim().to_string();
                    if steer.is_empty() {
                        continue;
                    }
                    pending_steers.push(steer.clone());
                    self.emit_status(format!(
                        "Steer input queued: {}",
                        summarize_text(&steer, 120)
                    ))
                    .await;
                }
            }

            match event {
                StreamEvent::MessageStart { message } => {
                    // Replace the running usage with the message's usage
                    // (input tokens + cache tokens arrive here for Anthropic
                    // / OpenAI). Mirrors the retired `handle_deepseek_turn`
                    // `usage = message.usage;`.
                    usage = message.usage;
                }
                StreamEvent::ContentBlockStart {
                    index,
                    content_block,
                } => {
                    let build = match content_block {
                        ContentBlockStart::Text { text } => {
                            // Block-lifecycle: a text block started — let the
                            // host frame the message before its first delta.
                            self.callback
                                .on_stream_delta(&StreamDelta::MessageStarted {
                                    index: index as usize,
                                })
                                .await;
                            BlockBuild::Text(text)
                        }
                        ContentBlockStart::Thinking { thinking } => {
                            self.callback
                                .on_stream_delta(&StreamDelta::ThinkingStarted {
                                    index: index as usize,
                                })
                                .await;
                            BlockBuild::Thinking(thinking)
                        }
                        ContentBlockStart::ToolUse {
                            id,
                            name,
                            input,
                            caller,
                        } => BlockBuild::ToolUse {
                            id,
                            name,
                            input_buf: String::new(),
                            start_input: input,
                            caller,
                        },
                        ContentBlockStart::ServerToolUse { id, name, input } => {
                            BlockBuild::ToolUse {
                                id,
                                name,
                                input_buf: String::new(),
                                start_input: input,
                                caller: None,
                            }
                        }
                    };
                    blocks.insert(index, build);
                }
                StreamEvent::ContentBlockDelta { index, delta } => {
                    if let Some(build) = blocks.get_mut(&index) {
                        match (build, delta) {
                            (BlockBuild::Text(buf), Delta::TextDelta { text }) => {
                                // Forward the delta to the callback before
                                // buffering — the host's UI streams as the
                                // model produces text.
                                self.callback
                                    .on_stream_delta(&StreamDelta::Text {
                                        index: index as usize,
                                        content: text.clone(),
                                    })
                                    .await;
                                buf.push_str(&text);
                            }
                            (BlockBuild::Thinking(buf), Delta::ThinkingDelta { thinking }) => {
                                self.callback
                                    .on_stream_delta(&StreamDelta::Thinking {
                                        index: index as usize,
                                        content: thinking.clone(),
                                    })
                                    .await;
                                buf.push_str(&thinking);
                            }
                            (
                                BlockBuild::ToolUse { input_buf, .. },
                                Delta::InputJsonDelta { partial_json },
                            ) => {
                                // Tool-input JSON is not user-visible — buffer
                                // for assembly, no callback emission.
                                input_buf.push_str(&partial_json);
                            }
                            // Delta/block kind mismatch — ignore (provider quirk).
                            _ => {}
                        }
                    }
                }
                StreamEvent::ContentBlockStop { index } => {
                    // Block-lifecycle: mark the block done. Production emits
                    // `ThinkingComplete` / `MessageComplete` here (and
                    // `ToolCallStarted` for tool blocks — the latter is
                    // absorbed ✅, see the `reduce_stream` doc / module doc). The block is
                    // looked up (not removed) so it stays available for
                    // `finalize_blocks` at stream end.
                    if let Some(build) = blocks.get(&index) {
                        match build {
                            BlockBuild::Thinking(_) => {
                                self.callback
                                    .on_stream_delta(&StreamDelta::ThinkingComplete {
                                        index: index as usize,
                                    })
                                    .await;
                            }
                            BlockBuild::Text(_) => {
                                self.callback
                                    .on_stream_delta(&StreamDelta::MessageComplete {
                                        index: index as usize,
                                    })
                                    .await;
                            }
                            BlockBuild::ToolUse {
                                id,
                                name,
                                input_buf,
                                start_input,
                                ..
                            } => {
                                // Finalize the tool input now that all
                                // `InputJsonDelta` fragments have arrived.
                                let finalized_input = finalize_tool_input(input_buf, start_input);

                                // Announce the tool call at stream-time —
                                // production fires `Event::ToolCallStarted` on
                                // `ContentBlockStop` for tool blocks (carrying
                                // the wire id) so the UI can show "calling X"
                                // before the tool actually executes. The
                                // `CallbackBridge` marks the id as announced so
                                // the execute-time `on_tool_start` skips
                                // re-emitting (dedup).
                                self.callback
                                    .on_stream_delta(&StreamDelta::ToolCallStarted {
                                        id: id.clone(),
                                        name: name.clone(),
                                        input: finalized_input.clone(),
                                    })
                                    .await;

                                // Early speculative dispatch: if the tool is
                                // early-start-safe (read-only, no approval),
                                // spawn it now so its result is ready by the
                                // tool loop (mirrors `handle_deepseek_turn`'s
                                // `early_tool_start_safe` + spawn at
                                // `ContentBlockStop`). `tokio::spawn` returns
                                // immediately (non-blocking); the stream keeps
                                // consuming. `Drop` on `EarlyToolTask` aborts
                                // an unreused task so nothing leaks.
                                if let Some(tool) = self.tools.get(name)
                                    && early_start_safe(&tool.capabilities())
                                {
                                    {
                                        let tool = Arc::clone(tool);
                                        let input = finalized_input.clone();
                                        let handle =
                                            tokio::spawn(async move { tool.run(input).await });
                                        early_tasks.insert(
                                            id.clone(),
                                            EarlyToolTask {
                                                name: name.clone(),
                                                input: finalized_input,
                                                handle: Some(handle),
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                StreamEvent::MessageDelta {
                    delta:
                        MessageDelta {
                            stop_reason: sr, ..
                        },
                    usage: delta_usage,
                } => {
                    if let Some(u) = delta_usage {
                        // Replace — the latest cumulative usage wins (mirrors
                        // the retired `handle_deepseek_turn`
                        // `if let Some(u) = delta_usage { usage = u; }`).
                        usage = u;
                    }
                    if sr.is_some() {
                        stop_reason = sr;
                    }
                }
                StreamEvent::MessageStop => break,
                StreamEvent::Ping => {}
            }
        }

        let content = finalize_blocks(blocks);
        StreamReduceOutcome::Complete {
            content,
            stop_reason,
            usage,
        }
    }

    /// (2) per-step post-stream seam — transparent stream retry + reactive
    /// capacity recovery.
    ///
    /// Drives `create_message_stream` + [`reduce_stream`] with a bounded
    /// transparent-retry loop. Only `StreamReduceOutcome::Empty` (the stream
    /// died before any content was produced) is eligible for retry — `Complete`
    /// and `Partial` both surface immediately. This closes the
    /// `accumulate_stream` bail-on-error gap: the old CORE reducer dropped
    /// partial blocks on the first erroring item, so the executor retried even
    /// when production would ship partial content (it tracks
    /// `any_content_received` and skips the retry once the user has seen
    /// output). The inline reducer now makes the same distinction.
    ///
    /// Re-issue the SAME request up to [`MAX_STREAM_RETRIES`] (3) times before
    /// propagating the failure. This mirrors `handle_deepseek_turn`'s outer
    /// "stream died with nothing" retry (`handle_deepseek_turn`). A healthy
    /// round resets the budget (`handle_deepseek_turn`), so a bad prior step
    /// doesn't carry over.
    ///
    /// **Reactive context-length recovery (seam 2):** a pre-stream error
    /// (`create_message_stream` returning `Err`) is classified via
    /// [`is_context_length_error_message`] before propagating. A
    /// context-length rejection triggers [`try_recover_context_overflow`] — if
    /// emergency compaction succeeds, the round returns
    /// [`StreamRoundOutcome::RecoveredContextOverflow`] so the caller restarts
    /// the step (the request snapshot picks up the compacted transcript),
    /// mirroring `handle_deepseek_turn`. Other pre-stream errors (connection /
    /// auth / timeout) and budget-exhausted / failed-recovery context-length
    /// errors propagate as a hard fail. A successful stream open resets the
    /// reactive recovery budget (mirrors `handle_deepseek_turn`). The retry and
    /// recovery are transparent to the [`Callback`](codesmith_agent::callback::Callback): `on_llm_start` /
    /// `on_llm_end` fire once per step, and a `Status` event is the only
    /// surfacing (matching production's silent re-issue / recovery).
    ///
    /// # Cancel-token checkpoints (absorbed)
    ///
    /// The cancel-token short-circuits are **absorbed** — Checkpoint B (stream-
    /// open race), Checkpoint C (`!cancelled` in the transparent-retry `Empty`
    /// arm, mirroring `should_transparently_retry_stream`), and Checkpoint D
    /// (post-stream gate, discarding even cleanly-completed content if the turn
    /// was cancelled mid-stream). All return [`StreamRoundOutcome::Interrupted`]
    /// so `run_inner` surfaces [`StopReason::Interrupted`](codesmith_agent::callback::StopReason::Interrupted). The loop-top gate
    /// (Checkpoint A in `run_inner`) bounds the capacity/reactive `continue`
    /// loops on a cancelled turn. Production's inner mid-flight retry (resetting
    /// the stream *inside* the event loop when no content was received yet,
    /// `handle_deepseek_turn`) is not replicated here; this executor uses the
    /// simpler outer retry (re-call `create_message_stream`). The two are
    /// functionally equivalent for the retry decision; the inner retry's
    /// advantage is avoiding a redundant `MessageStart` round-trip, which
    /// matters only for latency-sensitive production paths.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn stream_with_transparent_retry(
        &self,
        client: &LlmClientHandle,
        request: MessageRequest,
        stream_retry_attempts: &mut u32,
        context_recovery_attempts: &mut u8,
        history: &mut dyn ChatHistory,
        system: Option<&SystemPrompt>,
        early_tasks: &mut HashMap<String, EarlyToolTask>,
        pending_steers: &mut Vec<String>,
    ) -> Result<StreamRoundOutcome> {
        /// Cap on transparent stream retries — matches `handle_deepseek_turn`'s
        /// `MAX_STREAM_RETRIES` (3). One initial attempt + 3 retries = 4 total
        /// `create_message_stream` calls before the failure surfaces.
        const MAX_STREAM_RETRIES: u32 = 3;
        // Clone the token once per round so the cancel future owns a local
        // (not a `&self` borrow) — avoids borrow-checker conflicts with the
        // `self.emit_status` / `self.try_recover_context_overflow` calls in the
        // select arms. `CancellationToken::clone` is a cheap Arc bump.
        let cancel_token = self.cancel_token.clone();
        loop {
            // Checkpoint B — stream-open cancel race (mirrors
            // `handle_deepseek_turn`): race the cancel token against
            // `create_message_stream` so a cancelled turn aborts before the
            // stream even opens. `biased` so cancel wins if both are ready.
            // A pre-stream error may be a context-length rejection — classify
            // before propagating so reactive recovery (seam 2) can run. Only
            // mid-flight stream errors (from `reduce_stream`) retry
            // transparently; pre-stream errors are either recovered (restart
            // the step) or hard-failed.
            let stream = tokio::select! {
                biased;
                _ = async {
                    match &cancel_token {
                        Some(token) => token.cancelled().await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    self.emit_status("Request cancelled".to_string()).await;
                    return Ok(StreamRoundOutcome::Interrupted);
                }
                result = client.create_message_stream(request.clone()) => match result {
                    Ok(s) => {
                        // The provider accepted the request — the context was
                        // fine. Reset the reactive recovery budget (mirrors
                        // `handle_deepseek_turn`).
                        *context_recovery_attempts = 0;
                        s
                    }
                    Err(e) => {
                        let message = e.to_string();
                        if self
                            .try_recover_context_overflow(
                                client,
                                history,
                                system,
                                &message,
                                context_recovery_attempts,
                            )
                            .await
                        {
                            // Recovery succeeded — signal the caller to restart
                            // the step so the request snapshot picks up the
                            // compacted transcript (mirrors `handle_deepseek_turn`).
                            // Reset the stream-retry budget too (fresh step).
                            *stream_retry_attempts = 0;
                            return Ok(StreamRoundOutcome::RecoveredContextOverflow);
                        }
                        // Not a context-length error, no probe, budget exhausted,
                        // or recovery failed — hard-fail the turn.
                        return Err(anyhow::anyhow!(message));
                    }
                }
            };
            match self
                .reduce_stream(stream, early_tasks, pending_steers)
                .await
            {
                StreamReduceOutcome::Complete {
                    content,
                    stop_reason,
                    usage,
                } => {
                    // Checkpoint D — post-stream cancel gate (mirrors
                    // `handle_deepseek_turn`): even a cleanly-completed
                    // stream is discarded if the turn was cancelled mid-stream.
                    if self.is_cancelled() {
                        self.emit_status("Request cancelled".to_string()).await;
                        return Ok(StreamRoundOutcome::Interrupted);
                    }
                    // Healthy round → reset the retry budget so a bad prior
                    // step doesn't carry over (mirrors `handle_deepseek_turn`).
                    *stream_retry_attempts = 0;
                    return Ok(StreamRoundOutcome::Content {
                        content,
                        stop_reason,
                        usage,
                    });
                }
                StreamReduceOutcome::Partial {
                    content,
                    stop_reason,
                    error,
                    usage,
                } => {
                    // Checkpoint D — post-stream cancel gate. Same as Complete:
                    // if cancelled, discard the partial content and return
                    // `Interrupted`.
                    if self.is_cancelled() {
                        self.emit_status("Request cancelled".to_string()).await;
                        return Ok(StreamRoundOutcome::Interrupted);
                    }
                    // Stream died after content was received — surface the
                    // partial content, don't retry (the model has billed for
                    // output; retrying would double-bill and lose the partial
                    // turn). Reset the budget so a bad prior step doesn't
                    // carry over (mirrors `handle_deepseek_turn`).
                    *stream_retry_attempts = 0;
                    self.emit_status(format!(
                        "Stream interrupted after partial content; surfacing what was received: {error}"
                    ))
                    .await;
                    return Ok(StreamRoundOutcome::Content {
                        content,
                        stop_reason,
                        usage,
                    });
                }
                StreamReduceOutcome::Empty { error } => {
                    // Checkpoint C — transparent-retry `!cancelled` guard
                    // (mirrors `should_transparently_retry_stream` in
                    // `streaming.rs:81-87`): if the turn was cancelled, don't
                    // retry — return `Interrupted` (not `Err`).
                    if self.is_cancelled() {
                        self.emit_status("Request cancelled".to_string()).await;
                        return Ok(StreamRoundOutcome::Interrupted);
                    }
                    // Stream died before any content — safe to retry
                    // transparently (no output billed, nothing shown).
                    if *stream_retry_attempts < MAX_STREAM_RETRIES {
                        *stream_retry_attempts = stream_retry_attempts.saturating_add(1);
                        self.emit_status(format!(
                            "Connection interrupted; retrying ({}/{MAX_STREAM_RETRIES})",
                            *stream_retry_attempts,
                        ))
                        .await;
                        continue;
                    }
                    // Budget exhausted → surface the failure.
                    return Err(anyhow::anyhow!(error));
                }
            }
        }
    }

    /// Reactive seam-2 context-length recovery. When the provider rejects a
    /// request with a context-length error (classified via
    /// [`is_context_length_error_message`]), run emergency compaction via
    /// [`recover_context_overflow`] and signal the caller to restart the step
    /// — mirrors `handle_deepseek_turn`. Returns `true` only when **all** of:
    /// a [`CapacityProbe`](crate::engine::host_executor::CapacityProbe) is present, the error is a context-length
    /// rejection, the recovery budget (`*context_recovery_attempts` bounded by
    /// [`MAX_CONTEXT_RECOVERY_ATTEMPTS`]) allows, the model's budget is known,
    /// **and** [`recover_context_overflow`] succeeded.
    ///
    /// Any miss returns `false` so [`stream_with_transparent_retry`] hard-fails
    /// the turn: a non-context-length error (connection / auth / timeout), a
    /// missing probe (capacity disabled), an unknown model (no budget to judge
    /// recovery against), or budget exhaustion all propagate as a hard fail.
    /// On success the budget is incremented (mirrors `handle_deepseek_turn`); the
    /// reset lives in [`stream_with_transparent_retry`] on a successful stream
    /// open (`handle_deepseek_turn`).
    ///
    /// In practice a second reactive recovery in the same turn almost always
    /// fails: the first compaction leaves a short transcript (summary + recent
    /// tail), and re-summarizing the single older summary message is a no-op
    /// (no shrinkage ⇒ `recover_context_overflow` returns `false`). The
    /// [`MAX_CONTEXT_RECOVERY_ATTEMPTS`] cap is therefore a safety net that the
    /// preflight path (`run_capacity_preflight`) is far more likely to reach
    /// than this reactive path — matching production's defensive `MAX = 2`.
    async fn try_recover_context_overflow(
        &self,
        client: &LlmClientHandle,
        history: &mut dyn ChatHistory,
        system: Option<&SystemPrompt>,
        error_message: &str,
        context_recovery_attempts: &mut u8,
    ) -> bool {
        let Some(probe) = &self.capacity else {
            // No capacity probe ⇒ reactive recovery is disabled (mirrors the
            // preflight's `None` ⇒ `Proceed`, but here a pre-stream error has
            // no in-band fallback — hard-fail).
            return false;
        };
        if !is_context_length_error_message(error_message) {
            return false;
        }
        if *context_recovery_attempts >= MAX_CONTEXT_RECOVERY_ATTEMPTS {
            self.emit_status(format!(
                "Context length exceeded but recovery budget exhausted \
                 ({MAX_CONTEXT_RECOVERY_ATTEMPTS} attempts); failing the turn."
            ))
            .await;
            return false;
        }
        let Some(target_budget) =
            context_input_budget_for_provider(probe.api_provider, &probe.model)
        else {
            // Unknown model ⇒ no budget ⇒ can't judge recovery. Hard-fail
            // (mirrors the preflight's `None` budget ⇒ `Proceed`, but here the
            // provider already rejected — there's nothing to fall through to).
            return false;
        };
        let recovered = self
            .recover_context_overflow(
                client,
                history,
                system,
                target_budget,
                "provider context-length rejection",
            )
            .await;
        if recovered {
            *context_recovery_attempts = context_recovery_attempts.saturating_add(1);
        }
        recovered
    }
}

#[cfg(test)]
mod tests {
    use super::early_start_safe;
    use codesmith_agent::tools::ToolCapability;

    #[test]
    fn early_start_safe_allows_readonly() {
        assert!(early_start_safe(&[ToolCapability::ReadOnly]));
        // Network / Sandboxable don't disqualify a read-only tool.
        assert!(early_start_safe(&[
            ToolCapability::ReadOnly,
            ToolCapability::Network,
        ]));
        assert!(early_start_safe(&[
            ToolCapability::ReadOnly,
            ToolCapability::Sandboxable,
        ]));
    }

    #[test]
    fn early_start_safe_disqualifies_non_readonly() {
        // No ReadOnly at all ⇒ not safe.
        assert!(!early_start_safe(&[]), "empty caps");
        assert!(
            !early_start_safe(&[ToolCapability::Network]),
            "network only"
        );
        // ReadOnly + a disqualifier ⇒ not safe.
        assert!(!early_start_safe(&[
            ToolCapability::ReadOnly,
            ToolCapability::WritesFiles,
        ]));
        assert!(!early_start_safe(&[
            ToolCapability::ReadOnly,
            ToolCapability::ExecutesCode,
        ]));
        assert!(!early_start_safe(&[
            ToolCapability::ReadOnly,
            ToolCapability::RequiresApproval,
        ]));
    }
}
