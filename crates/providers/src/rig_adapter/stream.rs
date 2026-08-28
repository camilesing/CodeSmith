//! Translate a rig streaming completion into CodeSmith's `StreamEvent` lifecycle.
//!
//! rig yields [`StreamedAssistantContent`] items — flat text/reasoning deltas
//! and (possibly partial) tool calls. CodeSmith's engine consumes an
//! Anthropic-style block lifecycle: `MessageStart` → one or more
//! `ContentBlockStart` / `ContentBlockDelta` / `ContentBlockStop` triples →
//! `MessageDelta` (final usage) → `MessageStop`. This module owns that
//! re-framing with a small state machine driven by [`futures_util::stream::unfold`].
//!
//! Block-index policy: each opened block gets the next monotonically increasing
//! `u32` index, matching how Anthropic's real SSE tags content blocks. A block
//! is closed (`ContentBlockStop`) when a different content kind arrives or the
//! stream ends.
//!
//! Tool-call deltas (OpenAI-style: a `Name` delta then argument `Delta`
//! chunks) are the awkward case — CodeSmith has no "tool name delta" variant,
//! so the start is deferred until the name is known (or until the first
//! argument chunk forces it out with an empty name).
//!
//! OpenAI-compat providers deliver each tool call TWICE on the wire: the
//! streamed fragments plus an assembled complete `ToolCall` at
//! `finish_reason == tool_calls` (rig-core 0.39 does not suppress the latter
//! when deltas were emitted). The mapper reconciles the two by wire id
//! (`MapperState::tool_blocks_by_id`): the delta-built block is authoritative
//! and the trailing complete event is merged into it or suppressed — emitting
//! both would make the engine execute every streamed tool call twice.

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;

use anyhow::Result as AnyResult;
use codesmith_agent::models::{
    ContentBlockStart, Delta, MessageDelta, MessageResponse, StreamEvent, Usage,
};
use futures_util::StreamExt;
use futures_util::stream::unfold;
use rig_core::completion::GetTokenUsage;
use rig_core::completion::message::Text;
use rig_core::streaming::{StreamedAssistantContent, StreamingCompletionResponse};

use super::convert;

/// Wrap a rig streaming response in a CodeSmith `StreamEvent` stream ready to
/// hand back from `LlmClient::create_message_stream`.
pub(crate) fn map_rig_stream<R>(
    inner: StreamingCompletionResponse<R>,
    model: String,
) -> Pin<Box<dyn futures_util::Stream<Item = AnyResult<StreamEvent>> + Send + 'static>>
where
    R: Clone + Unpin + GetTokenUsage + Send + 'static,
{
    let state = MapperState {
        inner,
        model,
        started: false,
        usage_emitted: false,
        finished: false,
        pending: VecDeque::new(),
        next_index: 0,
        current: None,
        tool_blocks_by_id: HashMap::new(),
    };
    let s = unfold(state, |mut state| async move {
        loop {
            // 1. Drain anything the previous iteration queued.
            if let Some(ev) = state.pending.pop_front() {
                return Some((ev, state));
            }
            if state.finished {
                return None;
            }
            // 2. Emit MessageStart exactly once, before any content.
            if !state.started {
                state.started = true;
                let msg = MessageResponse {
                    id: super::synth_message_id(),
                    r#type: "message".to_string(),
                    role: "assistant".to_string(),
                    content: Vec::new(),
                    model: state.model.clone(),
                    stop_reason: None,
                    stop_sequence: None,
                    container: None,
                    usage: Usage::default(),
                };
                state
                    .pending
                    .push_back(Ok(StreamEvent::MessageStart { message: msg }));
                continue;
            }
            // 3. Pull the next rig item.
            match state.inner.next().await {
                None => {
                    // Stream ended. Close any open block, emit a final
                    // MessageDelta (with default usage if the provider never
                    // yielded a Final), then MessageStop.
                    if !state.usage_emitted {
                        state.close_current_block();
                        state.enqueue_message_delta(Usage::default());
                        state.usage_emitted = true;
                    }
                    state.pending.push_back(Ok(StreamEvent::MessageStop));
                    state.finished = true;
                    continue;
                }
                Some(Err(e)) => {
                    state.pending.push_back(Err(anyhow::Error::new(e)));
                    state.finished = true;
                    continue;
                }
                Some(Ok(item)) => {
                    state.handle_streamed_item(item);
                    continue;
                }
            }
        }
    });
    Box::pin(s)
}

/// The currently-open content block, if any. Drives `ContentBlockStart` /
/// `ContentBlockDelta` / `ContentBlockStop` emission.
#[derive(Debug)]
enum CurrentBlock {
    Text {
        index: u32,
    },
    Thinking {
        index: u32,
    },
    /// A tool-use block assembled from deltas. `started` is false until the
    /// `ContentBlockStart` has been emitted (deferred until the name is known
    /// or an argument chunk forces it).
    ToolUse {
        index: u32,
        id: String,
        name: Option<String>,
        started: bool,
    },
}

impl CurrentBlock {
    fn index(&self) -> u32 {
        match self {
            CurrentBlock::Text { index }
            | CurrentBlock::Thinking { index }
            | CurrentBlock::ToolUse { index, .. } => *index,
        }
    }
}

/// Bookkeeping for a delta-assembled tool block, keyed by wire tool-call id.
/// `input_delivered` records whether any `InputJsonDelta` was emitted for the
/// block — a trailing complete `ToolCall` for a block that never streamed
/// arguments must back-fill the authoritative payload (see
/// [`MapperState::open_complete_tool_use`]). Records outlive their block:
/// rig delivers the assembled `ToolCall` after the delta block may already
/// have been closed by later content.
#[derive(Debug, Clone)]
struct ToolBlockRecord {
    index: u32,
    input_delivered: bool,
}

struct MapperState<R>
where
    R: Clone + Unpin + GetTokenUsage + Send + 'static,
{
    inner: StreamingCompletionResponse<R>,
    model: String,
    started: bool,
    usage_emitted: bool,
    finished: bool,
    pending: VecDeque<AnyResult<StreamEvent>>,
    next_index: u32,
    current: Option<CurrentBlock>,
    tool_blocks_by_id: HashMap<String, ToolBlockRecord>,
}

impl<R> MapperState<R>
where
    R: Clone + Unpin + GetTokenUsage + Send + 'static,
{
    /// Emit `ContentBlockStop` for the open block (if any) and clear it. If the
    /// open block is a delta-assembled tool-use whose `ContentBlockStart` was
    /// deferred, emit that Start first so the block is well-formed.
    fn close_current_block(&mut self) {
        let Some(block) = self.current.take() else {
            return;
        };
        let index = block.index();
        if let CurrentBlock::ToolUse {
            id,
            name,
            started: false,
            ..
        } = &block
        {
            self.pending.push_back(Ok(StreamEvent::ContentBlockStart {
                index,
                content_block: ContentBlockStart::ToolUse {
                    id: id.clone(),
                    name: name.clone().unwrap_or_default(),
                    input: serde_json::Value::Null,
                    caller: None,
                },
            }));
        }
        self.pending
            .push_back(Ok(StreamEvent::ContentBlockStop { index }));
    }

    /// Open a text block: close the current one first if it isn't already text.
    fn open_text(&mut self) {
        if matches!(self.current, Some(CurrentBlock::Text { .. })) {
            return;
        }
        self.close_current_block();
        let index = self.next_index;
        self.next_index = self.next_index.saturating_add(1);
        self.pending.push_back(Ok(StreamEvent::ContentBlockStart {
            index,
            content_block: ContentBlockStart::Text {
                text: String::new(),
            },
        }));
        self.current = Some(CurrentBlock::Text { index });
    }

    /// Open a thinking block: close the current one first if it isn't already
    /// thinking.
    fn open_thinking(&mut self) {
        if matches!(self.current, Some(CurrentBlock::Thinking { .. })) {
            return;
        }
        self.close_current_block();
        let index = self.next_index;
        self.next_index = self.next_index.saturating_add(1);
        self.pending.push_back(Ok(StreamEvent::ContentBlockStart {
            index,
            content_block: ContentBlockStart::Thinking {
                thinking: String::new(),
            },
        }));
        self.current = Some(CurrentBlock::Thinking { index });
    }

    /// Open a tool-use block for a *complete* tool call (Start carries the full
    /// name + input), then immediately close it (Stop). No deltas.
    ///
    /// Reconciles with any delta-built block for the same wire id first (see
    /// the module doc: OpenAI-compat streams emit each call twice — fragments
    /// plus the assembled `ToolCall`). The delta block is authoritative; a
    /// second block here would double-execute the tool and leave an
    /// unpairable empty-id entry in the transcript:
    ///
    /// - current delta block, Start still deferred → emit the deferred Start
    ///   with the authoritative id/name/input, then close;
    /// - current delta block, already started → its fragments delivered the
    ///   same payload (rig assembles the complete call from those exact
    ///   fragments); close it;
    /// - closed delta block that never received argument deltas → back-fill
    ///   the authoritative arguments as a synthetic `InputJsonDelta` on the
    ///   closed block's index (the engine keeps the build alive after
    ///   `ContentBlockStop` and prefers `input_buf` at finalize time);
    /// - no delta block for this id → open a fresh block carrying the
    ///   complete payload (Start + immediate Stop).
    fn open_complete_tool_use(&mut self, id: String, name: String, input: serde_json::Value) {
        let matching_current = matches!(
            &self.current,
            Some(CurrentBlock::ToolUse { id: cur_id, .. }) if *cur_id == id
        );
        if let Some(record) = self.tool_blocks_by_id.get(&id).cloned() {
            if matching_current {
                let started = matches!(
                    &self.current,
                    Some(CurrentBlock::ToolUse { started: true, .. })
                );
                if !started {
                    if let Some(CurrentBlock::ToolUse {
                        name: buffered,
                        started,
                        ..
                    }) = self.current.as_mut()
                    {
                        *buffered = Some(name.clone());
                        // Mark started — close_current_block must not emit a
                        // second deferred Start for this block.
                        *started = true;
                    }
                    let index = record.index;
                    self.pending.push_back(Ok(StreamEvent::ContentBlockStart {
                        index,
                        content_block: ContentBlockStart::ToolUse {
                            id,
                            name,
                            input,
                            caller: None,
                        },
                    }));
                } else if !record.input_delivered && !input.is_null() {
                    // Started block that never streamed arguments: back-fill
                    // like the closed-block case below.
                    if let Ok(args) = serde_json::to_string(&input) {
                        self.enqueue_input_json_delta(record.index, args);
                    }
                }
                self.close_current_block();
                return;
            }
            // Closed delta block for this id — suppress the duplicate block.
            // Back-fill arguments if none streamed (the block closed while
            // still deferred, e.g. Name(A) followed by Name(B)).
            if !record.input_delivered
                && !input.is_null()
                && let Ok(args) = serde_json::to_string(&input)
            {
                self.enqueue_input_json_delta(record.index, args);
            }
            return;
        }
        self.close_current_block();
        let index = self.next_index;
        self.next_index = self.next_index.saturating_add(1);
        self.pending.push_back(Ok(StreamEvent::ContentBlockStart {
            index,
            content_block: ContentBlockStart::ToolUse {
                id,
                name,
                input,
                caller: None,
            },
        }));
        self.pending
            .push_back(Ok(StreamEvent::ContentBlockStop { index }));
    }

    /// Ensure a delta-assembled tool-use block is current for `id`, returning
    /// its index. Opens a new block (closing the previous) if the id differs.
    /// The `ContentBlockStart` is deferred until the name is known or an
    /// argument chunk forces it (see [`Self::start_tool_use_if_needed`]).
    fn ensure_tool_use_delta(&mut self, id: String) {
        if let Some(CurrentBlock::ToolUse { id: cur_id, .. }) = &self.current
            && *cur_id == id
        {
            return;
        }
        self.close_current_block();
        let index = self.next_index;
        self.next_index = self.next_index.saturating_add(1);
        self.current = Some(CurrentBlock::ToolUse {
            index,
            id: id.clone(),
            name: None,
            started: false,
        });
        self.tool_blocks_by_id.insert(
            id,
            ToolBlockRecord {
                index,
                input_delivered: false,
            },
        );
    }

    /// Emit the deferred `ContentBlockStart` for the current delta tool-use
    /// block, if not already started, using the buffered name (or empty).
    /// Returns the block's index (or `u32::MAX` if no tool-use block is
    /// current — unreachable from the dispatcher).
    fn start_tool_use_if_needed(&mut self) -> u32 {
        let Some(CurrentBlock::ToolUse {
            index,
            id,
            name,
            started,
            ..
        }) = self.current.as_mut()
        else {
            return u32::MAX;
        };
        if !*started {
            *started = true;
            let name = name.clone().unwrap_or_default();
            let id = id.clone();
            let index = *index;
            self.pending.push_back(Ok(StreamEvent::ContentBlockStart {
                index,
                // The real wire id, NOT empty — the engine keys speculative
                // early-start tasks and tool_use/tool_result pairing by it.
                content_block: ContentBlockStart::ToolUse {
                    id,
                    name,
                    input: serde_json::Value::Null,
                    caller: None,
                },
            }));
        }
        *index
    }

    fn enqueue_text_delta(&mut self, text: String) {
        if let Some(CurrentBlock::Text { index }) = &self.current {
            self.pending.push_back(Ok(StreamEvent::ContentBlockDelta {
                index: *index,
                delta: Delta::TextDelta { text },
            }));
        }
    }

    fn enqueue_thinking_delta(&mut self, thinking: String) {
        if let Some(CurrentBlock::Thinking { index }) = &self.current {
            self.pending.push_back(Ok(StreamEvent::ContentBlockDelta {
                index: *index,
                delta: Delta::ThinkingDelta { thinking },
            }));
        }
    }

    fn enqueue_input_json_delta(&mut self, index: u32, partial_json: String) {
        self.pending.push_back(Ok(StreamEvent::ContentBlockDelta {
            index,
            delta: Delta::InputJsonDelta { partial_json },
        }));
    }

    fn enqueue_message_delta(&mut self, usage: Usage) {
        self.pending.push_back(Ok(StreamEvent::MessageDelta {
            delta: MessageDelta {
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
            },
            usage: Some(usage),
        }));
    }

    /// Dispatch a single rig streamed item to the right block transition /
    /// delta emission. May queue zero or more `StreamEvent`s.
    fn handle_streamed_item(&mut self, item: StreamedAssistantContent<R>) {
        match item {
            StreamedAssistantContent::Text(Text { text, .. }) => {
                self.open_text();
                self.enqueue_text_delta(text);
            }
            StreamedAssistantContent::Reasoning(reasoning) => {
                self.open_thinking();
                self.enqueue_thinking_delta(reasoning.display_text());
            }
            StreamedAssistantContent::ReasoningDelta { reasoning, .. } => {
                self.open_thinking();
                self.enqueue_thinking_delta(reasoning);
            }
            StreamedAssistantContent::ToolCall { tool_call, .. } => {
                self.open_complete_tool_use(
                    tool_call.id,
                    tool_call.function.name,
                    tool_call.function.arguments,
                );
            }
            StreamedAssistantContent::ToolCallDelta { id, content, .. } => {
                use rig_core::streaming::ToolCallDeltaContent;
                match content {
                    ToolCallDeltaContent::Name(name) => {
                        self.ensure_tool_use_delta(id);
                        // Buffer the name; don't force-start yet — a later
                        // argument chunk or the block close will emit Start.
                        if let Some(CurrentBlock::ToolUse {
                            name: n,
                            started: false,
                            ..
                        }) = &mut self.current
                        {
                            *n = Some(name);
                        }
                    }
                    ToolCallDeltaContent::Delta(args) => {
                        self.ensure_tool_use_delta(id);
                        let index = self.start_tool_use_if_needed();
                        if let Some(CurrentBlock::ToolUse { id: cur_id, .. }) = &self.current
                            && let Some(record) = self.tool_blocks_by_id.get_mut(cur_id)
                        {
                            record.input_delivered = true;
                        }
                        self.enqueue_input_json_delta(index, args);
                    }
                }
            }
            StreamedAssistantContent::Final(response) => {
                // Final carries usage via GetTokenUsage. Close the open block,
                // then emit the message-level delta before MessageStop.
                self.close_current_block();
                let usage = convert::usage_to_codesmith(&response.token_usage());
                self.enqueue_message_delta(usage);
                self.usage_emitted = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use rig_core::completion::CompletionError;
    use rig_core::completion::GetTokenUsage;
    use rig_core::completion::Usage as RigUsage;
    use rig_core::streaming::{
        RawStreamingChoice, RawStreamingToolCall, StreamingCompletionResponse, ToolCallDeltaContent,
    };

    /// Minimal final-response type satisfying the mapper's `R` bound
    /// (`Clone + Unpin + GetTokenUsage + Send + 'static`).
    #[derive(Clone)]
    struct FakeFinal(RigUsage);

    impl GetTokenUsage for FakeFinal {
        fn token_usage(&self) -> RigUsage {
            self.0
        }
    }

    /// Build a mapper input from the exact item sequence rig's
    /// OpenAI-compat providers emit (raw choices, pre-aggregation — the
    /// `StreamingCompletionResponse` Stream impl turns these into the
    /// `StreamedAssistantContent` items `map_rig_stream` consumes).
    fn raw_stream(
        items: Vec<RawStreamingChoice<FakeFinal>>,
    ) -> StreamingCompletionResponse<FakeFinal> {
        let it = futures_util::stream::iter(items.into_iter().map(Ok::<_, CompletionError>));
        StreamingCompletionResponse::stream(Box::pin(it))
    }

    async fn collect(resp: StreamingCompletionResponse<FakeFinal>) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        let mut s = map_rig_stream(resp, "test-model".to_string());
        while let Some(item) = s.next().await {
            if let Ok(ev) = item {
                out.push(ev);
            }
        }
        out
    }

    /// (index, id, name) of every tool-use `ContentBlockStart`.
    fn tool_use_starts(events: &[StreamEvent]) -> Vec<(u32, String, String)> {
        events
            .iter()
            .filter_map(|ev| match ev {
                StreamEvent::ContentBlockStart {
                    index,
                    content_block: ContentBlockStart::ToolUse { id, name, .. },
                } => Some((*index, id.clone(), name.clone())),
                _ => None,
            })
            .collect()
    }

    fn delta(id: &str, content: ToolCallDeltaContent) -> RawStreamingChoice<FakeFinal> {
        RawStreamingChoice::ToolCallDelta {
            id: id.to_string(),
            internal_call_id: format!("i_{id}"),
            content,
        }
    }

    /// rig emits Name delta + argument deltas, then the assembled complete
    /// ToolCall at finish. ONE logical call must yield ONE tool-use block
    /// carrying the real wire id — never a second, never an empty id.
    #[tokio::test]
    async fn delta_streamed_tool_call_emits_single_block_with_real_id() {
        let resp = raw_stream(vec![
            delta(
                "call_abc",
                ToolCallDeltaContent::Name("get_weather".to_string()),
            ),
            delta(
                "call_abc",
                ToolCallDeltaContent::Delta("{\"city\":".to_string()),
            ),
            delta(
                "call_abc",
                ToolCallDeltaContent::Delta("\"Paris\"}".to_string()),
            ),
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call_abc".to_string(),
                "get_weather".to_string(),
                serde_json::json!({"city": "Paris"}),
            )),
            RawStreamingChoice::FinalResponse(FakeFinal(RigUsage::new())),
        ]);
        let events = collect(resp).await;
        assert_eq!(
            tool_use_starts(&events),
            vec![(0, "call_abc".to_string(), "get_weather".to_string())],
            "one logical streamed tool call must materialize exactly one block"
        );
    }

    /// Name delta streams, arguments never do (parameterless call or a
    /// gateway that only sends arguments in the finish event): the trailing
    /// complete ToolCall must emit the deferred Start carrying the
    /// authoritative id and full parsed input.
    #[tokio::test]
    async fn complete_after_name_only_carries_full_input() {
        let resp = raw_stream(vec![
            delta("call_1", ToolCallDeltaContent::Name("list_dir".to_string())),
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call_1".to_string(),
                "list_dir".to_string(),
                serde_json::json!({"path": "."}),
            )),
            RawStreamingChoice::FinalResponse(FakeFinal(RigUsage::new())),
        ]);
        let events = collect(resp).await;
        assert_eq!(
            tool_use_starts(&events),
            vec![(0, "call_1".to_string(), "list_dir".to_string())]
        );
        for ev in &events {
            if let StreamEvent::ContentBlockStart {
                content_block: ContentBlockStart::ToolUse { id, input, .. },
                ..
            } = ev
            {
                assert_eq!(id, "call_1");
                assert_eq!(*input, serde_json::json!({"path": "."}));
            }
        }
    }

    /// A complete ToolCall with no deltas at all (non-streaming gateway or
    /// eviction path) still opens exactly one block with the real id.
    #[tokio::test]
    async fn complete_without_any_delta_opens_one_block() {
        let resp = raw_stream(vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call_2".to_string(),
                "bash".to_string(),
                serde_json::json!({"command": "ls"}),
            )),
            RawStreamingChoice::FinalResponse(FakeFinal(RigUsage::new())),
        ]);
        let events = collect(resp).await;
        assert_eq!(
            tool_use_starts(&events),
            vec![(0, "call_2".to_string(), "bash".to_string())]
        );
    }

    /// Parallel calls (A deltas, B deltas, then completes A and B in wire
    /// order): two blocks with distinct real ids — the out-of-order
    /// completes must reconcile with the already-closed delta blocks, not
    /// open duplicates.
    #[tokio::test]
    async fn parallel_tool_calls_stay_distinct() {
        let resp = raw_stream(vec![
            delta(
                "call_a",
                ToolCallDeltaContent::Name("read_file".to_string()),
            ),
            delta(
                "call_a",
                ToolCallDeltaContent::Delta("{\"p\":1}".to_string()),
            ),
            delta(
                "call_b",
                ToolCallDeltaContent::Name("read_file".to_string()),
            ),
            delta(
                "call_b",
                ToolCallDeltaContent::Delta("{\"p\":2}".to_string()),
            ),
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call_a".to_string(),
                "read_file".to_string(),
                serde_json::json!({"p": 1}),
            )),
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call_b".to_string(),
                "read_file".to_string(),
                serde_json::json!({"p": 2}),
            )),
            RawStreamingChoice::FinalResponse(FakeFinal(RigUsage::new())),
        ]);
        let events = collect(resp).await;
        assert_eq!(
            tool_use_starts(&events),
            vec![
                (0, "call_a".to_string(), "read_file".to_string()),
                (1, "call_b".to_string(), "read_file".to_string()),
            ]
        );
    }

    /// A delta block closed while still deferred (Name(A), Name(B), …) that
    /// never received argument deltas gets its authoritative arguments
    /// back-filled via a synthetic InputJsonDelta on its (already stopped)
    /// block index — the engine keeps the build alive after
    /// ContentBlockStop and prefers `input_buf` at finalize time.
    #[tokio::test]
    async fn closed_unstarted_block_gets_backfilled_input() {
        let resp = raw_stream(vec![
            delta("call_a", ToolCallDeltaContent::Name("tool_a".to_string())),
            delta("call_b", ToolCallDeltaContent::Name("tool_b".to_string())),
            delta(
                "call_b",
                ToolCallDeltaContent::Delta("{\"q\":9}".to_string()),
            ),
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call_a".to_string(),
                "tool_a".to_string(),
                serde_json::json!({"p": 7}),
            )),
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call_b".to_string(),
                "tool_b".to_string(),
                serde_json::json!({"q": 9}),
            )),
            RawStreamingChoice::FinalResponse(FakeFinal(RigUsage::new())),
        ]);
        let events = collect(resp).await;
        assert_eq!(
            tool_use_starts(&events),
            vec![
                (0, "call_a".to_string(), "tool_a".to_string()),
                (1, "call_b".to_string(), "tool_b".to_string()),
            ]
        );
        let backfill = events.iter().find_map(|ev| match ev {
            StreamEvent::ContentBlockDelta {
                index: 0,
                delta: Delta::InputJsonDelta { partial_json },
            } => Some(partial_json.clone()),
            _ => None,
        });
        let backfill = backfill.expect("call_a must receive a synthetic InputJsonDelta");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&backfill).unwrap(),
            serde_json::json!({"p": 7})
        );
    }
}
