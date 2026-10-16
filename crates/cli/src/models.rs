//! Re-export of [`codesmith_agent::models`] (extracted in Phase 6 §6a).
//!
//! The canonical home for API request/response models is now the
//! `codesmith_agent` crate. This shim keeps `crate::models::<item>` paths in
//! the CLI working; the explicit list below pins exactly which items this
//! crate consumes, so upstream renames fail here instead of at distant call
//! sites and the eventual removal is a mechanical check.
// TODO(shim-removal): remove this shim once the remaining call sites are
// rewired onto `codesmith_agent::models` directly.
pub use codesmith_agent::models::{
    ContentBlock, Message, MessageRequest, MessageResponse, SystemBlock, SystemPrompt, Tool, Usage,
    compaction_threshold_for_model, compaction_threshold_for_window, context_window_for_model,
};
// Consumed only through `crate::models::<item>` from cfg(test) modules
// (llm_client::mock, config tests, runtime_api tests, engine tests).
#[cfg(test)]
pub use codesmith_agent::models::{
    ContentBlockStart, DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS, Delta,
    LEGACY_MODEL_CONTEXT_WINDOW_TOKENS, MessageDelta, StreamEvent, ToolCaller,
};
