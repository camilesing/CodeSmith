//! Operations submitted by the UI to the core engine.
//!
//! These operations flow from the TUI to the engine via a channel,
//! allowing the UI to remain responsive while the engine processes requests.

use crate::compaction::CompactionConfig;
use crate::models::{Message, SystemPrompt};
use crate::tui::app::AppMode;
use crate::tui::approval::ApprovalMode;
use std::path::PathBuf;

/// Compaction mode for manual /compact commands.
#[derive(Debug, Clone, PartialEq)]
pub enum CompactMode {
    /// Full compaction (default behavior).
    Full,
    /// Partial compaction preserving prefix cache (From direction).
    From { pivot_index: usize },
    /// Partial compaction sacrificing prefix cache (UpTo direction).
    UpTo { pivot_index: usize },
    /// Session-memory-based compaction (KoD/MEMORY.md summary).
    Memory,
}

/// Operations that can be submitted to the engine.
#[derive(Debug, Clone)]
pub enum Op {
    /// Send a message to the AI
    SendMessage {
        content: String,
        mode: AppMode,
        model: String,
        goal_objective: Option<String>,
        /// Reasoning-effort tier: `"off" | "low" | "medium" | "high" | "max"`.
        /// `None` lets the provider apply its default.
        reasoning_effort: Option<String>,
        /// True when the user selected auto thinking, even though the UI sends
        /// a concrete per-turn value to the model API.
        reasoning_effort_auto: bool,
        /// True when the user selected auto model routing.
        auto_model: bool,
        allow_shell: bool,
        trust_mode: bool,
        auto_approve: bool,
        approval_mode: ApprovalMode,
        translation_enabled: bool,
        show_thinking: bool,
        /// Tool restriction from custom slash command frontmatter.
        /// `None` means the current turn may use the normal tool set.
        allowed_tools: Option<Vec<String>>,
    },

    /// Cancel the current request
    #[allow(dead_code)]
    CancelRequest,

    /// Approve a tool call that requires permission
    #[allow(dead_code)]
    ApproveToolCall { id: String },

    /// Deny a tool call that requires permission
    #[allow(dead_code)]
    DenyToolCall { id: String },

    /// Spawn a sub-agent
    #[allow(dead_code)]
    SpawnSubAgent { prompt: String },

    /// List current sub-agents and their status
    ListSubAgents,

    /// Change the operating mode
    #[allow(dead_code)]
    ChangeMode { mode: AppMode },

    /// Update the model being used
    #[allow(dead_code)]
    SetModel { model: String },

    /// Update auto-compaction settings
    SetCompaction { config: CompactionConfig },

    /// Sync engine session state (used for resume/load)
    SyncSession {
        session_id: Option<String>,
        messages: Vec<Message>,
        system_prompt: Option<SystemPrompt>,
        system_prompt_override: bool,
        model: String,
        workspace: PathBuf,
    },

    /// Run context compaction immediately (default full mode).
    CompactContext,

    /// Run context compaction with a specific mode.
    CompactContextWithMode { mode: CompactMode },

    /// Run agent-driven context purging.
    PurgeContext,

    /// Edit the last user message: remove the last user+assistant exchange
    /// from the session, then re-send with the new content.
    #[allow(dead_code)]
    EditLastTurn { new_message: String },

    /// Shutdown the engine
    Shutdown,

    // === Background Task Operations ===

    /// Start a shell command in background.
    #[allow(dead_code)]
    StartBackgroundShell {
        command: String,
        cwd: Option<PathBuf>,
        timeout_secs: Option<u64>,
    },

    /// Cancel a background task by unified id.
    #[allow(dead_code)]
    CancelBackgroundTask { id: String },

    /// List all background tasks across all subsystems.
    #[allow(dead_code)]
    ListBackgroundTasks,

    /// Poll a specific background task for incremental output.
    #[allow(dead_code)]
    PollBackgroundTask { id: String },

    /// Background the currently foreground shell task.
    #[allow(dead_code)]
    BackgroundCurrentShell,

    /// Background all foreground tasks.
    #[allow(dead_code)]
    BackgroundAll,

    /// Trigger a memory consolidation (dream) task.
    #[allow(dead_code)]
    StartDreamTask { memory_path: Option<PathBuf> },

    /// Team inbox dispatch received from the inbox poller.
    TeamInboxDispatch {
        dispatch: crate::tools::team::InboxDispatch,
    },
}
