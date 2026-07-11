//! # Observation callbacks
//!
//! The LangChain `Callbacks` analog: a trait an executor invokes at each phase
//! of the agent loop (LLM start/end, tool start/end, step, completion) so the
//! run is **observable** without the core depending on a host's UI/event
//! channel. All methods default to no-ops, so a host wires only the hooks it
//! cares about.
//!
//! This is deliberately a Rust trait (in-process), distinct from the existing
//! `mpsc::Sender<Event>` push channel and the shell-command `HookHost` in
//! `codesmith-agent-runtime` — those are bridged onto this trait in a later
//! ROADMAP §E slice. See `ARCHITECTURE.md` ("Framework-core agent seam").

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::models::{ContentBlock, MessageRequest};
use crate::tools::{ToolError, ToolResult};

/// Why an [`crate::executor::AgentExecutor`] run stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// The model produced an assistant turn with no tool calls — the run is
    /// finished.
    NoToolCalls,
    /// The step budget (`max_steps`) was exhausted mid-tool-loop.
    MaxSteps,
    /// The run aborted with an error.
    Error(String),
}

/// A UI-relevant streaming event, emitted by the inline stream reducer in real
/// time (not buffered until stream end). Two families of variant:
///
/// - **Content deltas** ([`StreamDelta::Text`] / [`StreamDelta::Thinking`]):
///   incremental text/thinking chunks that flow to the host's UI as they
///   arrive. Tool-input JSON deltas are **not** emitted here (they're assembled
///   into the final [`ContentBlock::ToolUse`] block, which is not user-visible
///   until `on_llm_end`).
/// - **Block-lifecycle markers** ([`StreamDelta::MessageStarted`] /
///   [`StreamDelta::ThinkingStarted`] / [`StreamDelta::ThinkingComplete`] /
///   [`StreamDelta::MessageComplete`]): block-boundary signals synthesized at
///   `ContentBlockStart` / `ContentBlockStop` (no content payload). They let
///   the host's UI frame a block before its first delta arrives and mark it
///   done when its last delta lands — matching the production
///   `Event::MessageStarted` / `Event::ThinkingStarted` / `Event::ThinkingComplete`
///   / `Event::MessageComplete` emissions. The stream-time `ToolCallStarted`
///   for tool blocks is **not** here yet — it's deferred to the early-tool-start
///   slice (which needs the tool catalog to validate input before announcing
///   the call, and a bridge refactor to avoid duplicating the execute-time
///   `on_tool_start` emission).
///
/// The `index` is the content-block index from the wire `StreamEvent`, so a
/// host can correlate deltas with their block (matching the production
/// `Event::MessageDelta { index, .. }` / `Event::ThinkingDelta { index, .. }`).
#[derive(Debug, Clone)]
pub enum StreamDelta {
    /// Incremental assistant text (maps to `Event::MessageDelta`).
    Text {
        /// Content-block index from the wire stream event.
        index: usize,
        /// The incremental text chunk.
        content: String,
    },
    /// Incremental reasoning/thinking text (maps to `Event::ThinkingDelta`).
    Thinking {
        /// Content-block index from the wire stream event.
        index: usize,
        /// The incremental thinking chunk.
        content: String,
    },
    /// A text block started (maps to `Event::MessageStarted`). Fired at
    /// `ContentBlockStart::Text`, before the block's first [`StreamDelta::Text`]
    /// delta — lets the host frame the message before content lands.
    MessageStarted {
        /// Content-block index from the wire stream event.
        index: usize,
    },
    /// A thinking block started (maps to `Event::ThinkingStarted`). Fired at
    /// `ContentBlockStart::Thinking`, before the block's first
    /// [`StreamDelta::Thinking`] delta.
    ThinkingStarted {
        /// Content-block index from the wire stream event.
        index: usize,
    },
    /// A thinking block completed (maps to `Event::ThinkingComplete`). Fired at
    /// `ContentBlockStop` for a thinking block, after its last
    /// [`StreamDelta::Thinking`] delta.
    ThinkingComplete {
        /// Content-block index from the wire stream event.
        index: usize,
    },
    /// A text block completed (maps to `Event::MessageComplete`). Fired at
    /// `ContentBlockStop` for a text block, after its last [`StreamDelta::Text`]
    /// delta.
    MessageComplete {
        /// Content-block index from the wire stream event.
        index: usize,
    },
}

/// No-op boxed future, the default body for every [`Callback`] method.
///
/// `'static` so it satisfies any `'a` the caller ties to `&self` + the params.
fn noop<'a>() -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
    Box::pin(async {})
}

/// Observation hooks for an agent run. Every method has a default no-op; impl
/// only the ones you need. Methods are async (boxed future, matching
/// [`crate::llm_client::LlmClient`]) so a host can do I/O (telemetry, tracing)
/// without blocking the executor.
///
/// The single lifetime `'a` ties `&self` and each borrowed argument together, so
/// an impl may forward the request/response/content **by reference** (zero-copy)
/// — the returned future borrows them for `'a`.
pub trait Callback: Send + Sync {
    /// Fired just before the LLM stream request is issued.
    fn on_llm_start<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let _ = request;
        noop()
    }

    /// Fired after the stream closed, with the assembled assistant content
    /// blocks (text / thinking / tool_use).
    fn on_llm_end<'a>(
        &'a self,
        content: &'a [ContentBlock],
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let _ = content;
        noop()
    }

    /// Fired before a tool is executed.
    fn on_tool_start<'a>(
        &'a self,
        name: &'a str,
        input: &'a serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let _ = (name, input);
        noop()
    }

    /// Fired after a tool returns, with its outcome.
    fn on_tool_end<'a>(
        &'a self,
        name: &'a str,
        result: &'a Result<ToolResult, ToolError>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let _ = (name, result);
        noop()
    }

    /// Fired at the end of each loop step (after tool results are fed back).
    fn on_step<'a>(&'a self, step: u32) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let _ = step;
        noop()
    }

    /// Fired once when the run stops, with the [`StopReason`].
    fn on_complete<'a>(
        &'a self,
        reason: &'a StopReason,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let _ = reason;
        noop()
    }

    /// Fired for each streaming text/thinking delta, in real time as the LLM
    /// stream produces them (before [`Callback::on_llm_end`]). Default no-op;
    /// a host that wants live UI streaming implements this to push deltas to
    /// its event channel. The `index` correlates consecutive deltas that
    /// belong to the same content block.
    ///
    /// This is the framework seam for the production stream-reduction loop's
    /// `Event::MessageDelta` / `Event::ThinkingDelta` emission — without it,
    /// the executor buffers the whole stream before surfacing any content,
    /// which means the user sees nothing until the stream completes (and a
    /// mid-flight stream error loses all partial content).
    fn on_stream_delta<'a>(
        &'a self,
        delta: &'a StreamDelta,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let _ = delta;
        noop()
    }
}

/// A [`Callback`] that does nothing — the default for simple embeds.
#[derive(Debug, Default)]
pub struct NoopCallback;

impl Callback for NoopCallback {}

/// Fan-out [`Callback`]: forwards every hook to each member, in registration
/// order. Use it when a host wants several observers (e.g. tracing + a UI
/// bridge) on one executor. Forwards every argument **by reference** (no clone).
#[derive(Default)]
pub struct CallbackSet {
    callbacks: Vec<Arc<dyn Callback>>,
}

impl CallbackSet {
    /// Create an empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a callback member.
    pub fn push(&mut self, callback: Arc<dyn Callback>) {
        self.callbacks.push(callback);
    }
}

impl Callback for CallbackSet {
    fn on_llm_start<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let cbs = self.callbacks.clone();
        Box::pin(async move {
            for cb in &cbs {
                cb.on_llm_start(request).await;
            }
        })
    }

    fn on_llm_end<'a>(
        &'a self,
        content: &'a [ContentBlock],
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let cbs = self.callbacks.clone();
        Box::pin(async move {
            for cb in &cbs {
                cb.on_llm_end(content).await;
            }
        })
    }

    fn on_tool_start<'a>(
        &'a self,
        name: &'a str,
        input: &'a serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let cbs = self.callbacks.clone();
        Box::pin(async move {
            for cb in &cbs {
                cb.on_tool_start(name, input).await;
            }
        })
    }

    fn on_tool_end<'a>(
        &'a self,
        name: &'a str,
        result: &'a Result<ToolResult, ToolError>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let cbs = self.callbacks.clone();
        Box::pin(async move {
            for cb in &cbs {
                cb.on_tool_end(name, result).await;
            }
        })
    }

    fn on_step<'a>(&'a self, step: u32) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let cbs = self.callbacks.clone();
        Box::pin(async move {
            for cb in &cbs {
                cb.on_step(step).await;
            }
        })
    }

    fn on_complete<'a>(
        &'a self,
        reason: &'a StopReason,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let cbs = self.callbacks.clone();
        Box::pin(async move {
            for cb in &cbs {
                cb.on_complete(reason).await;
            }
        })
    }

    fn on_stream_delta<'a>(
        &'a self,
        delta: &'a StreamDelta,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let cbs = self.callbacks.clone();
        Box::pin(async move {
            for cb in &cbs {
                cb.on_stream_delta(delta).await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_callback_defaults_are_callable() {
        let cb = NoopCallback;
        cb.on_step(0).await;
        cb.on_complete(&StopReason::NoToolCalls).await;
        cb.on_stream_delta(&StreamDelta::Text {
            index: 0,
            content: "hello".to_string(),
        })
        .await;
        cb.on_stream_delta(&StreamDelta::MessageStarted { index: 0 })
            .await;
        cb.on_stream_delta(&StreamDelta::MessageComplete { index: 0 })
            .await;
    }

    #[tokio::test]
    async fn callback_set_fans_out() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc as StdArc;

        struct Counter(StdArc<AtomicU32>);
        impl Callback for Counter {
            fn on_step<'a>(&'a self, _step: u32) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
                let c = self.0.clone();
                Box::pin(async move {
                    c.fetch_add(1, Ordering::SeqCst);
                })
            }
        }

        let count = StdArc::new(AtomicU32::new(0));
        let mut set = CallbackSet::new();
        set.push(Arc::new(Counter(count.clone())));
        set.push(Arc::new(Counter(count.clone())));
        set.on_step(1).await;
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }
}
