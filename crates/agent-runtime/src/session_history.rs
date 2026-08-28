//! Bridge the production `Session` transcript onto the framework-core
//! [`ChatHistory`].
//!
//! The framework-core [`ChatHistory`] trait (LangChain `Memory` analog, in
//! `codesmith-agent`) is the executor's transcript view: an [`AgentExecutor`]
//! reads prior turns via [`ChatHistory::messages`] and appends assistant turns
//! and tool results via [`ChatHistory::push`]. The production [`Session`] in
//! this crate owns the real conversation bytes (`session.messages`) alongside
//! compaction / working-set / cycle state. [`SessionChatHistory`] closes that
//! gap: it borrows a `&mut Session` and exposes the `messages` vec through the
//! trait, so a framework executor sees the (already-compacted) message list the
//! host owns.
//!
//! This is the third and last host→framework bridge (after `ToolSpecAdapter`
//! and `CallbackBridge`) — the "land the bridge" step of ROADMAP §E. The
//! production `Engine` migration onto `AgentExecutor` is done (slice 20 §E
//! cutover): `Engine::handle_send_message` constructs this adapter per turn
//! and passes it to `HostAgentExecutor`.
//!
//! ## Scope
//!
//! `ChatHistory` only exposes the `messages` vec; compaction / working_set /
//! cycle state stay on `Session` and are reached by the host executor's
//! guardrails directly, not through this trait. [`ChatHistory::push`] is a pure
//! `session.messages.push` — it deliberately does **not** run working-set
//! observation; that is a host guardrail's job, not the memory adapter's
//! (consistent with `codesmith_agent::memory`'s module docs: "compaction /
//! summarization is out of scope here").
//!
//! See `ARCHITECTURE.md` ("Framework-core agent seam") and `ROADMAP.md` §E.

use codesmith_agent::memory::ChatHistory;
use tokio::sync::mpsc;

use crate::events::Event;
use crate::models::Message;
use crate::session::Session;

/// A [`ChatHistory`] backed by a borrowed [`Session`]'s `messages` vec.
///
/// Construct one per run with `&mut session`; the executor mutates the
/// transcript in place through the trait, so the host keeps ownership of where
/// the bytes live. The borrow ties the adapter to the caller's session for the
/// run's duration (matching [`AgentExecutor::run`](codesmith_agent::executor::AgentExecutor::run)'s
/// `&'a mut dyn ChatHistory`).
pub struct SessionChatHistory<'a> {
    session: &'a mut Session,
    /// UI event channel for `Event::SessionUpdated` on each push (slice 20 §E).
    /// `None` in embeds/tests that don't surface session updates — the
    /// executor's ~73 existing tests use [`new`](Self::new) (=> `None`) and are
    /// unchanged. `Some` in the production wire-in
    /// ([`new_with_event_tx`](Self::new_with_event_tx)) so every
    /// assistant/tool-result/steer/LSP push refreshes the host UI.
    event_tx: Option<mpsc::Sender<Event>>,
}

impl<'a> SessionChatHistory<'a> {
    /// Borrow a `Session`'s transcript for use as a [`ChatHistory`], with no
    /// `SessionUpdated` emission on push (embed/test path — unchanged behavior
    /// for the executor's existing tests).
    #[must_use]
    pub fn new(session: &'a mut Session) -> Self {
        Self {
            session,
            event_tx: None,
        }
    }

    /// Like [`new`](Self::new) but emits `Event::SessionUpdated` on every
    /// [`ChatHistory::push`] (production wire-in path). The host's
    /// `emit_session_updated().await` still runs once pre-turn for a guaranteed
    /// refresh; this covers the N mid-turn pushes (assistant, tool-result,
    /// steer, LSP flush) so the UI tracks the transcript live.
    #[must_use]
    pub fn new_with_event_tx(
        session: &'a mut Session,
        event_tx: Option<mpsc::Sender<Event>>,
    ) -> Self {
        Self { session, event_tx }
    }
}

impl<'a> ChatHistory for SessionChatHistory<'a> {
    fn messages(&self) -> &[Message] {
        &self.session.messages
    }

    fn push(&mut self, message: Message) {
        // `Session::add_message` is a plain `push` (no side effects); call it
        // for parity with the rest of the engine, which routes message appends
        // through `add_message`.
        self.session.add_message(message);
        // Best-effort UI refresh (slice 20 §E). `try_send` (sync, not
        // `.await`) because `ChatHistory::push` is a sync trait method.
        // Drop-on-full is acceptable: the post-turn `TurnComplete` carries the
        // final transcript, and `handle_send_message` keeps its
        // `emit_session_updated` pre-turn for a guaranteed refresh.
        if let Some(tx) = &self.event_tx {
            let _ = tx.try_send(Event::SessionUpdated {
                session_id: self.session.id.clone(),
                messages: self.session.messages.clone(),
                system_prompt: self.session.system_prompt.clone(),
                model: self.session.model.clone(),
                workspace: self.session.workspace.clone(),
            });
        }
    }

    fn clear(&mut self) {
        self.session.messages.clear();
    }

    fn len(&self) -> usize {
        self.session.messages.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ContentBlock;
    use std::path::PathBuf;

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
    fn session_history_backs_session_messages() {
        let mut sess = fresh_session();
        let mut hist = SessionChatHistory::new(&mut sess);
        assert!(hist.is_empty());
        hist.push(user_text("hello"));
        hist.push(user_text("world"));
        assert_eq!(hist.len(), 2);
        assert_eq!(hist.messages().len(), 2);

        // The pushes landed on the underlying Session — proof of real
        // delegation, not a copy.
        assert_eq!(sess.messages.len(), 2);
        assert_eq!(sess.messages[0].role, "user");
        assert_eq!(sess.messages[1].role, "user");
    }

    #[test]
    fn session_history_clear_empties_session() {
        let mut sess = fresh_session();
        let mut hist = SessionChatHistory::new(&mut sess);
        hist.push(user_text("x"));
        assert!(!hist.is_empty());
        hist.clear();
        assert!(hist.is_empty());
        // Clear propagated to the Session.
        assert!(sess.messages.is_empty());
    }
}
