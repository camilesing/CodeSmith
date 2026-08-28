//! # Conversation memory abstraction
//!
//! The LangChain `Memory` analog: a trait over the conversation transcript so
//! an [`crate::executor::AgentExecutor`] stays host-agnostic. The executor
//! reads prior turns via [`ChatHistory::messages`] and appends assistant turns
//! and tool results via [`ChatHistory::push`]; where the bytes live (in-memory
//! `Vec`, a DB, a session file) is the host's choice.
//!
//! Compaction / summarization is out of scope here — that stays in
//! `codesmith-agent-runtime`'s `Session` + `compaction` modules. A host that
//! wants auto-compaction backs [`ChatHistory`] with its `Session` and lets the
//! executor see the (already-compacted) message list. See `ROADMAP.md` §E.

use crate::models::Message;

/// Read/write view over the conversation transcript (LangChain `Memory` analog).
///
/// Dyn-safe so an [`crate::executor::AgentExecutor`] can hold a `&mut dyn
/// ChatHistory` without naming the backing store.
pub trait ChatHistory: Send + Sync {
    /// Borrow the current message list (API format: alternating user/assistant
    /// turns, with tool results as `role:"user"` `ToolResult` blocks).
    fn messages(&self) -> &[Message];

    /// Append a message — an assistant turn or a tool-result user turn. The
    /// executor is responsible for the role/turn structure; the backing store
    /// only persists order.
    fn push(&mut self, message: Message);

    /// Drop the entire transcript.
    fn clear(&mut self);

    /// Current transcript length.
    fn len(&self) -> usize;

    /// Whether the transcript is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Trivial `Vec`-backed [`ChatHistory`]. The default for tests and simple
/// embeds; production hosts back this with their `Session`.
#[derive(Debug, Default)]
pub struct VecChatHistory {
    messages: Vec<Message>,
}

impl VecChatHistory {
    /// Create an empty history.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl ChatHistory for VecChatHistory {
    fn messages(&self) -> &[Message] {
        &self.messages
    }

    fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    fn clear(&mut self) {
        self.messages.clear();
    }

    fn len(&self) -> usize {
        self.messages.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ContentBlock, Message};

    fn user_text(text: &str) -> Message {
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    #[test]
    fn push_and_read_back() {
        let mut h = VecChatHistory::new();
        assert!(h.is_empty());
        h.push(user_text("hello"));
        h.push(user_text("world"));
        assert_eq!(h.len(), 2);
        assert_eq!(h.messages().len(), 2);
    }

    #[test]
    fn clear_empties_transcript() {
        let mut h = VecChatHistory::new();
        h.push(user_text("x"));
        h.clear();
        assert!(h.is_empty());
        assert!(h.messages().is_empty());
    }
}
