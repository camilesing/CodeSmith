//! Events emitted by the core engine to the UI.
//!
//! These events flow from the engine to the TUI via a channel,
//! enabling non-blocking, real-time updates.

use std::{path::PathBuf, sync::Arc};

use serde_json::Value;

use crate::background_task::{
    BackgroundTaskNotification, BackgroundTaskSummary, BackgroundTaskType,
};
use crate::coherence::CoherenceState;
use crate::error_taxonomy::ErrorEnvelope;
use crate::models::{Message, SystemPrompt, Tool, Usage};
use crate::subagent::SubAgentResult;
use crate::telemetry::{RedactedAnalyticsMetadata, VerifiedAnalyticsMetadata};
use crate::user_input::UserInputRequest;
use codesmith_tools::{ToolError, ToolResult};

/// Final status for a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnOutcomeStatus {
    Completed,
    Interrupted,
    Failed,
}

/// Events emitted by the engine to update the UI.
#[derive(Debug, Clone)]
pub enum Event {
    // === Streaming Events ===
    /// A new message block has started
    MessageStarted {
        #[allow(dead_code)]
        index: usize,
    },

    /// Incremental text content delta
    MessageDelta {
        #[allow(dead_code)]
        index: usize,
        content: String,
    },

    /// Message block completed
    MessageComplete {
        #[allow(dead_code)]
        index: usize,
    },

    /// Thinking block started
    ThinkingStarted {
        #[allow(dead_code)]
        index: usize,
    },

    /// Incremental thinking content delta
    ThinkingDelta {
        #[allow(dead_code)]
        index: usize,
        content: String,
    },

    /// Thinking block completed
    ThinkingComplete {
        #[allow(dead_code)]
        index: usize,
    },

    // === Tool Events ===
    /// Tool call initiated
    ToolCallStarted {
        id: String,
        name: String,
        input: Value,
    },

    /// Tool execution progress (for long-running tools)
    #[allow(dead_code)]
    ToolCallProgress { id: String, output: String },

    /// Tool call completed
    ToolCallComplete {
        id: String,
        name: String,
        result: Result<ToolResult, ToolError>,
    },

    // === Turn Lifecycle ===
    /// A new turn has started (user sent a message)
    TurnStarted { turn_id: String },

    /// The turn is complete (no more tool calls)
    TurnComplete {
        usage: Usage,
        status: TurnOutcomeStatus,
        error: Option<String>,
        /// Tool catalog sent with this turn's model request.
        tool_catalog: Option<Vec<Tool>>,
        /// API base URL used by this turn's client.
        base_url: Option<String>,
    },

    /// Context compaction started.
    CompactionStarted {
        id: String,
        auto: bool,
        message: String,
    },

    /// Context compaction completed.
    CompactionCompleted {
        id: String,
        auto: bool,
        message: String,
        /// Number of messages before compaction.
        #[allow(dead_code)]
        messages_before: Option<usize>,
        /// Number of messages after compaction.
        #[allow(dead_code)]
        messages_after: Option<usize>,
    },

    /// Context purge started.
    PurgeStarted {
        /// Status message for display.
        message: String,
    },

    /// Context purge completed.
    PurgeCompleted {
        /// Number of messages before purge.
        messages_before: usize,
        /// Number of messages after purge.
        messages_after: usize,
        /// How many messages were removed.
        removed_count: usize,
        /// How many replace operations were applied.
        replaced_count: usize,
        /// Summary message for display.
        message: String,
    },

    /// Context purge failed.
    PurgeFailed { message: String },

    /// Context compaction failed.
    CompactionFailed {
        id: String,
        auto: bool,
        message: String,
    },

    /// Checkpoint-restart cycle boundary advanced (issue #124). The previous
    /// cycle has already been archived to disk; the engine has swapped its
    /// in-memory message buffer for the seed messages of cycle `to`.
    /// Carries the full briefing record so the UI can populate
    /// `app.cycle_briefings` for `/cycle <n>`.
    CycleAdvanced {
        from: u32,
        to: u32,
        briefing: crate::cycle_manager::CycleBriefing,
    },

    /// Capacity decision telemetry.
    #[allow(dead_code)]
    CapacityDecision {
        session_id: VerifiedAnalyticsMetadata,
        turn_id: VerifiedAnalyticsMetadata,
        h_hat: f64,
        c_hat: f64,
        slack: f64,
        min_slack: f64,
        violation_ratio: f64,
        p_fail: f64,
        risk_band: VerifiedAnalyticsMetadata,
        action: VerifiedAnalyticsMetadata,
        cooldown_blocked: bool,
        reason: VerifiedAnalyticsMetadata,
    },

    /// Capacity intervention telemetry.
    #[allow(dead_code)]
    CapacityIntervention {
        session_id: VerifiedAnalyticsMetadata,
        turn_id: VerifiedAnalyticsMetadata,
        action: VerifiedAnalyticsMetadata,
        before_prompt_tokens: usize,
        after_prompt_tokens: usize,
        compaction_size_reduction: usize,
        // `replay_outcome` embeds summarized tool outputs (e.g.
        // "output_mismatch: original='...' replay='...'") which may carry
        // code or file content, so it CANNOT be honestly marked
        // `VerifiedAnalyticsMetadata`. It is run through
        // `RedactedAnalyticsMetadata::redact` at the construction site
        // (Plan 06 / 6.3) so only the sanitized value reaches the sink.
        replay_outcome: Option<RedactedAnalyticsMetadata>,
        replan_performed: bool,
    },

    /// Capacity memory persistence failure telemetry.
    #[allow(dead_code)]
    CapacityMemoryPersistFailed {
        session_id: VerifiedAnalyticsMetadata,
        turn_id: VerifiedAnalyticsMetadata,
        action: VerifiedAnalyticsMetadata,
        // `error` is a summarized IO-error string which may include path
        // fragments, so it CANNOT be honestly marked
        // `VerifiedAnalyticsMetadata`. It is run through
        // `RedactedAnalyticsMetadata::redact` at the construction site
        // (Plan 06 / 6.3) so only the sanitized value reaches the sink.
        error: RedactedAnalyticsMetadata,
    },

    /// Plain-language session coherence state.
    CoherenceState {
        state: CoherenceState,
        label: String,
        description: String,
        reason: String,
    },

    // === Sub-Agent Events ===
    /// A sub-agent has been spawned
    AgentSpawned { id: String, prompt: String },

    /// Sub-agent progress update
    AgentProgress { id: String, status: String },

    /// Sub-agent completed
    AgentComplete { id: String, result: String },

    /// Sub-agent listing
    AgentList { agents: Vec<SubAgentResult> },

    /// Structured sub-agent mailbox envelope (issue #128). Carries the
    /// monotonic seq + the typed `MailboxMessage` so the UI can route each
    /// envelope to the correct in-transcript card.
    SubAgentMailbox {
        seq: u64,
        message: crate::mailbox::MailboxMessage,
    },

    // === System Events ===
    /// An error occurred
    Error {
        envelope: ErrorEnvelope,
        #[allow(dead_code)]
        recoverable: bool,
    },

    /// Status message for UI display
    Status { message: String },

    /// Pause terminal input events (for interactive subprocesses).
    PauseEvents {
        /// Optional one-shot notification fired after the UI has actually
        /// released the terminal to the child process.
        ack: Option<Arc<tokio::sync::Notify>>,
    },

    /// Resume terminal input events after subprocess completion
    ResumeEvents,

    /// Request user approval for a tool call
    ApprovalRequired {
        id: String,
        tool_name: String,
        description: String,
        /// Tool parameters for approval display. Carried on the event so the
        /// TUI does not need to reconstruct them from `pending_tool_uses`.
        input: Value,
        /// Exact-argument fingerprint, used to scope *denials* (#1617).
        approval_key: String,
        /// Lossy / arity-aware fingerprint, used to scope *approvals* so an
        /// "approve for session" covers later flag variants (v0.8.37).
        approval_grouping_key: String,
        /// The model's explanation of intent before invoking write tools (#2381).
        /// Displayed in the approval view so users understand *why* the change
        /// is being made before reviewing *what* will change.
        intent_summary: Option<String>,
    },

    /// Request user input for a tool call
    UserInputRequired {
        id: String,
        request: UserInputRequest,
    },

    /// Authoritative API conversation state from the engine session.
    ///
    /// The UI receives granular display events, but those are not always a
    /// lossless representation of the API transcript. DeepSeek can emit
    /// reasoning directly followed by tool calls without a visible assistant
    /// text block, and that assistant message still has to be persisted for
    /// later `reasoning_content` replay.
    SessionUpdated {
        session_id: String,
        messages: Vec<Message>,
        system_prompt: Option<SystemPrompt>,
        model: String,
        workspace: PathBuf,
    },

    /// Request user decision after sandbox denial
    #[allow(dead_code)]
    ElevationRequired {
        tool_id: String,
        tool_name: String,
        command: Option<String>,
        denial_reason: String,
        blocked_network: bool,
        blocked_write: bool,
    },

    // === Prefix-Cache Stability Events ===
    /// The prefix (system prompt + tool specs) changed between turns,
    /// which invalidates DeepSeek's KV prefix cache. Carries diagnostics
    /// for the TUI to surface.
    PrefixCacheChange {
        /// Human-readable description of what changed.
        description: String,
        /// Whether the system prompt component changed.
        system_prompt_changed: bool,
        /// Whether the tool set component changed.
        tools_changed: bool,
        /// Overall prefix stability percentage (100 = fully stable).
        stability_pct: u32,
        /// True when the prefix actually changed (cache invalidated).
        /// False for routine stable-check heartbeats.
        changed: bool,
        /// Current pinned prefix combined hash (SHA-256, 64 hex chars).
        /// Carried so `/cache stats` can surface it without reaching
        /// into the engine's PrefixStabilityManager.
        pinned_combined_hash: String,
    },

    // === Background Task Events ===
    /// A background task has been registered and started.
    #[allow(dead_code)]
    BackgroundTaskStarted {
        id: String,
        task_type: BackgroundTaskType,
        description: String,
    },

    /// Incremental output from a background task.
    #[allow(dead_code)]
    BackgroundTaskProgress {
        id: String,
        output_delta: String,
        /// True if a stall was detected on this progress update.
        stall_detected: bool,
    },

    /// A background task completed successfully.
    #[allow(dead_code)]
    BackgroundTaskComplete {
        id: String,
        task_type: BackgroundTaskType,
        description: String,
        result_summary: Option<String>,
        duration_ms: Option<u64>,
    },

    /// A background task failed or was killed.
    #[allow(dead_code)]
    BackgroundTaskFailed {
        id: String,
        task_type: BackgroundTaskType,
        description: String,
        error: String,
    },

    /// Background task notification ready for injection into
    /// the conversation as a synthetic message.
    #[allow(dead_code)]
    BackgroundTaskNotification {
        notification: BackgroundTaskNotification,
    },

    /// Background task listing result.
    #[allow(dead_code)]
    BackgroundTaskList { tasks: Vec<BackgroundTaskSummary> },
}

impl Event {
    /// Create an error event from a categorized envelope. The envelope's own
    /// `recoverable` flag controls whether the UI flips into offline mode.
    pub fn error(envelope: ErrorEnvelope) -> Self {
        let recoverable = envelope.recoverable;
        Event::Error {
            envelope,
            recoverable,
        }
    }

    /// Create a new status event
    pub fn status(message: impl Into<String>) -> Self {
        Event::Status {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Finding 5a: the safe analytics fields of capacity events are
    /// `VerifiedAnalyticsMetadata` (ids + enum-derived labels + controlled
    /// `reason`), which forces conscious construction and still renders via
    /// `Display` for the TUI's `format!("{action}")` call sites.
    #[test]
    fn capacity_decision_safe_fields_render_via_display() {
        let ev = Event::CapacityDecision {
            session_id: VerifiedAnalyticsMetadata::verified("sess-ephemeral"),
            turn_id: VerifiedAnalyticsMetadata::verified("turn-1"),
            h_hat: 1.0,
            c_hat: 0.5,
            slack: 0.5,
            min_slack: 0.2,
            violation_ratio: 0.0,
            p_fail: 0.1,
            risk_band: VerifiedAnalyticsMetadata::verified("low"),
            action: VerifiedAnalyticsMetadata::verified("none"),
            cooldown_blocked: false,
            reason: VerifiedAnalyticsMetadata::verified("low_risk_no_intervention"),
        };
        match ev {
            Event::CapacityDecision {
                session_id,
                risk_band,
                action,
                reason,
                ..
            } => {
                assert_eq!(format!("{session_id}"), "sess-ephemeral");
                assert_eq!(format!("{risk_band}"), "low");
                assert_eq!(format!("{action}"), "none");
                assert_eq!(format!("{reason}"), "low_risk_no_intervention");
            }
            _ => unreachable!("constructed as CapacityDecision"),
        }
    }

    /// The path/code-bearing fields (`replay_outcome`, `error`) are typed as
    /// `RedactedAnalyticsMetadata`: they embed summarized tool output / IO
    /// errors that cannot be honestly marked `VerifiedAnalyticsMetadata`, so
    /// they are sanitized via `redact` at the construction site (Plan 06/6.3).
    #[test]
    fn capacity_intervention_replay_outcome_is_redacted() {
        let ev = Event::CapacityIntervention {
            session_id: VerifiedAnalyticsMetadata::verified("sess-ephemeral"),
            turn_id: VerifiedAnalyticsMetadata::verified("turn-1"),
            action: VerifiedAnalyticsMetadata::verified("replay"),
            before_prompt_tokens: 1000,
            after_prompt_tokens: 800,
            compaction_size_reduction: 200,
            replay_outcome: Some(RedactedAnalyticsMetadata::redact(
                "output_mismatch: original='fn main(){}'",
            )),
            replan_performed: false,
        };
        match ev {
            Event::CapacityIntervention { replay_outcome, .. } => {
                let outcome = replay_outcome.expect("replay_outcome present");
                let s = outcome.as_str();
                assert!(s.contains("output_mismatch"), "got: {s}");
                // the quoted code span must be scrubbed by `redact`.
                assert!(!s.contains("fn main()"), "code leaked through redact: {s}");
            }
            _ => unreachable!("constructed as CapacityIntervention"),
        }
    }
}
