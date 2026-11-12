//! Durable child-session journal — the sub-agent conversation record.
//!
//! Seam: [`ChildSessionJournalFactory`] is the definition the engine side
//! consumes (carried on [`EngineConfig`](crate::engine_config::EngineConfig)
//! and propagated by the host into every `SubAgentRuntime`); the CLI's
//! runtime-thread store is the provider
//! (`crates/cli/src/runtime_threads/subagent_journal.rs`). The child turn
//! ([`run_child_turn`](crate::engine::child_turn::run_child_turn)) records
//! its own user/assistant/tool/approval/usage/finish events through
//! [`ChildSessionJournal::record`] while the turn runs — never into the
//! parent's transcript.
//!
//! Replacement semantics: a child session is a record of one run, not a
//! resumable conversation. The provider opens a fresh child identity per
//! spawn; a process restart marks an unfinished child `interrupted` and keeps
//! its transcript. There is deliberately no resume path.
//!
//! Model visibility: this record is UI/replay only. A child's messages never
//! enter the parent model request; the parent receives the existing
//! completion card. A host that later folds child content into a model
//! request must add a session event first (the repository's
//! "model-visible means logged" rule).
//!
//! Known limitations:
//! - The provider is optional. Embedded engines and the interactive REPL
//!   host that do not build a runtime-thread store leave the seam `None`;
//!   their children keep today's in-memory transcript handle only.
//! - Script-authored `spawn_child` children run through `run_child_turn`
//!   without a manager-spawned identity, so they are not presented as child
//!   sessions in the catalog.
//! - Assistant text is recorded per model response, not per stream delta:
//!   a mid-turn reconnect sees the last settled step, not partial tokens.

use std::path::PathBuf;

use codesmith_agent::models::{ContentBlock, Usage};

/// One in-process child's identity, known by the host at spawn time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildSessionDescriptor {
    /// Durable child session id (the agent id).
    pub agent_id: String,
    /// Immediate parent: the root thread id for a direct child, else the
    /// spawning sub-agent's id.
    pub parent_id: String,
    /// The child's agent type (`general`, `explore`, ...).
    pub agent_type: String,
    /// Spawn depth (1 = a direct child of the main turn).
    pub depth: u32,
    /// Model-facing display name.
    pub name: String,
    /// Whether the child accepts follow-up input while it runs.
    pub mode: ChildSessionMode,
    /// The routed model the child's requests name.
    pub model: String,
    /// The child's workspace (the parent's).
    pub workspace: PathBuf,
    /// The opening assignment text shown as the child's first user message
    /// (a forked child's inherited prefix is not replayed).
    pub assignment: String,
}

/// Whether the child can accept follow-up input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildSessionMode {
    /// Foreground / one-shot: a terminal child. The UI composer is read-only.
    OneShot,
    /// A background child with a live `agent_send_input` channel.
    Continuable,
}

impl ChildSessionMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OneShot => "one-shot",
            Self::Continuable => "continuable",
        }
    }
}

/// How a child session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildSessionStatus {
    Completed,
    Cancelled,
    Failed(String),
}

impl ChildSessionStatus {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed(_) => "failed",
        }
    }
}

/// One journal record for a child session. Events arrive in run order; the
/// provider folds them into its durable thread/turn/item record.
#[derive(Debug, Clone)]
pub enum ChildSessionEvent {
    /// The run started; the provider materializes the child identity, an
    /// in-progress turn, and the opening assignment message.
    Started,
    /// A user message delivered to the child: its opening assignment or a
    /// follow-up `agent_send_input`.
    UserMessage { text: String },
    /// One model response's settled content blocks (text / thinking / tool
    /// calls). The provider records assistant messages and tool-call items.
    AssistantMessage { content: Vec<ContentBlock> },
    /// A tool call began. `id` is the wire call id; `input` is the model's
    /// arguments.
    ToolStarted {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// A tool call returned. The provider matches the most recent open call
    /// of `name` (parallel calls of the same name settle in start order).
    ToolFinished {
        name: String,
        ok: bool,
        content: String,
    },
    /// The child's out-of-workspace path question reached the parent's
    /// approval router.
    ApprovalRequested {
        id: String,
        tool_name: String,
        path: PathBuf,
    },
    /// The parent's approval router answered that question.
    ApprovalDecided { id: String, allowed: bool },
    /// Token usage for the run (reported once at the end).
    Usage(Usage),
    /// The run reached a terminal state. The provider closes the turn and
    /// the child's status before the turn lease is released.
    Finished {
        status: ChildSessionStatus,
        final_text: Option<String>,
        usage: Usage,
    },
}

/// Definition: open one child-session journal for a spawn.
///
/// The engine-side [`ChildTurnSpec`](crate::engine::child_turn::ChildTurnSpec)
/// carries the opened journal; the factory itself rides the host runtime so
/// grandchildren open their own sessions through the same provider.
pub trait ChildSessionJournalFactory: Send + Sync + std::fmt::Debug {
    /// Open a journal for `descriptor`, or `None` when this host keeps no
    /// child-session record. Opening is cheap; the first `Started` event
    /// materializes the durable identity.
    fn open(
        &self,
        descriptor: ChildSessionDescriptor,
    ) -> Option<std::sync::Arc<dyn ChildSessionJournal>>;
}

/// Definition: where a child turn sends its journal events.
#[async_trait::async_trait]
pub trait ChildSessionJournal: Send + Sync + std::fmt::Debug {
    /// Append one event. Failures are the provider's to log: journaling is
    /// observational and must never fail the child's turn.
    async fn record(&self, event: ChildSessionEvent);
}
