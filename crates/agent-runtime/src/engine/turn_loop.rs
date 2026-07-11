//! Main streaming turn loop for the engine.
//!
//! Extracted from `core/engine.rs` for issue #74. This module keeps the
//! existing per-turn orchestration intact: request construction, streaming
//! event handling, tool planning/execution, LSP post-edit hooks, capacity
//! checkpoints, and loop termination.
//!
//! After slice 20 §E (`handle_deepseek_turn` retirement) this module is a
//! residual: `EarlyToolResult` / `EarlyToolTask` are retained only for the
//! type references in `dispatch.rs`, and `messages_with_turn_metadata` /
//! `subagent_completion_runtime_message` remain live. The retained structs'
//! fields are unread until a follow-up slice re-wires speculative dispatch;
//! `#![allow(dead_code)]` silences those until then.

#![allow(dead_code)]

use super::*;

#[derive(Debug)]
struct EarlyToolResult {
    result: Result<ToolResult, ToolError>,
    elapsed: Duration,
}

#[derive(Debug)]
pub struct EarlyToolTask {
    name: String,
    input: serde_json::Value,
    handle: tokio::task::JoinHandle<EarlyToolResult>,
}

impl Engine {
    pub fn messages_with_turn_metadata(&self) -> Vec<Message> {
        // `<turn_meta>` is stored on user-text messages when the message is
        // appended. Do not rewrite historical messages at request time: doing
        // so makes the API prefix differ from the bytes sent in earlier turns
        // and destroys DeepSeek's KV prefix cache reuse.
        self.session.messages.clone()
    }
}

pub(crate) fn subagent_completion_runtime_message(payload: &str) -> Message {
    // Role is "user", not "system": some OpenAI-compatible backends apply a
    // strict chat template (e.g. vLLM serving Qwen3) that requires any system
    // message to be messages[0]. A system message appended mid-conversation
    // makes the template raise "System message must be at the beginning",
    // which surfaces as a 400 BadRequest and breaks the whole sub-agent
    // hand-off in the parent turn. The `visibility="internal"` tag already
    // tells the model this is a runtime event rather than user input, so the
    // role carries no semantic weight here — only template-compatibility cost.
    Message {
        role: "user".to_string(),
        content: vec![ContentBlock::Text {
            text: format!(
                "<codesmith:runtime_event kind=\"subagent_completion\" visibility=\"internal\">\n\
This is an internal runtime event, not user input. Use the sub-agent completion \
data below to continue coordinating the current task. Do not tell the user they \
pasted sentinels, do not explain the sentinel protocol, and do not quote the raw \
XML unless the user explicitly asks to debug sub-agent internals.\n\n\
{payload}\n\
</codesmith:runtime_event>"
            ),
            cache_control: None,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subagent_completion_handoff_is_internal_user_message() {
        let message = subagent_completion_runtime_message(
            "Build passed\n<codesmith:subagent.done>{\"agent_id\":\"agent_a\"}</codesmith:subagent.done>",
        );

        // Must be "user", not "system": a system message appended mid-stream
        // trips strict chat templates (vLLM/Qwen3) into a 400 BadRequest
        // ("System message must be at the beginning"). The internal-event
        // framing lives in the text + visibility tag, not the role.
        assert_eq!(message.role, "user");
        let text = match &message.content[0] {
            ContentBlock::Text { text, .. } => text,
            other => panic!("expected text block, got {other:?}"),
        };
        assert!(text.contains("internal runtime event, not user input"));
        assert!(text.contains("Do not tell the user they pasted sentinels"));
        assert!(text.contains("<codesmith:subagent.done>"));
        assert!(text.contains("Build passed"));
    }
}
