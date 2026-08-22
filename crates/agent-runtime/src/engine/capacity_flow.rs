//! Capacity-controller checkpoints and interventions for the engine loop.
//!
//! Extracted from `core/engine.rs` for issue #74. The main turn loop still
//! decides when checkpoints run; this module owns the guardrail policy side
//! effects, replay verification, canonical-state persistence, and event
//! emission helpers.

use super::*;

use std::path::{Path, PathBuf};

use codesmith_agent::memory::ChatHistory;

use crate::error_taxonomy::ErrorCategory;
use crate::tool_dispatch::ToolDispatcher;
use crate::working_set::WorkingSet;

use crate::models::context_window_for_model;
use crate::telemetry::{RedactedAnalyticsMetadata, VerifiedAnalyticsMetadata};

/// Count tool-call blocks (ToolUse + ToolResult) in the most recent
/// `message_window` messages of the transcript.
///
/// Extracted as a free function so the `CapacityGateProbe` (executor path)
/// can build the same observation the `Engine` builds, without needing `&self`.
pub(crate) fn recent_tool_call_count(messages: &[Message], message_window: usize) -> usize {
    messages
        .iter()
        .rev()
        .take(message_window)
        .map(|msg| {
            msg.content
                .iter()
                .filter(|block| {
                    matches!(
                        block,
                        ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. }
                    )
                })
                .count()
        })
        .sum()
}

/// Count unique reference IDs in the recent transcript window: tool-use IDs,
/// tool-result IDs, path-like tokens in text blocks, recent tool-call IDs
/// from the current turn, and working-set top paths.
///
/// Extracted as a free function so the `CapacityGateProbe` (executor path)
/// can build the same observation the `Engine` builds, without needing `&self`.
pub(crate) fn recent_unique_reference_count(
    messages: &[Message],
    message_window: usize,
    recent_tool_call_ids: &[String],
    working_set: &WorkingSet,
) -> usize {
    let mut refs = std::collections::HashSet::new();
    for msg in messages.iter().rev().take(message_window) {
        for block in &msg.content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    refs.insert(id.clone());
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    refs.insert(tool_use_id.clone());
                }
                ContentBlock::Text { text, .. } => {
                    for token in text.split_whitespace() {
                        if token.contains('/') || token.contains('.') {
                            refs.insert(
                                token
                                    .trim_matches(|c: char| ",.;:()[]{}".contains(c))
                                    .to_string(),
                            );
                        }
                    }
                }
                ContentBlock::Thinking { .. }
                | ContentBlock::Image { .. }
                | ContentBlock::ServerToolUse { .. }
                | ContentBlock::ToolSearchToolResult { .. }
                | ContentBlock::CodeExecutionToolResult { .. } => {}
            }
        }
    }
    for id in recent_tool_call_ids.iter().rev().take(8) {
        refs.insert(id.clone());
    }
    for path in working_set.top_paths(8) {
        refs.insert(path);
    }
    refs.retain(|item| !item.is_empty());
    refs.len()
}

/// Find the last user message carrying a `Text` block and the last user
/// message carrying a `[verification replay]` `ToolResult` block.
///
/// Extracted as a free function so both the mid-loop executor path
/// (`reset_history_to_latest_user_and_verified`, via `ChatHistory`) and the
/// post-`run` `Engine::apply_verify_and_replan` (via `&mut Vec<Message>`)
/// share one extraction source (§E slice 3a).
pub(crate) fn latest_user_and_verified(
    messages: &[Message],
) -> (Option<Message>, Option<Message>) {
    let latest_user = messages
        .iter()
        .rev()
        .find(|msg| {
            msg.role == "user"
                && msg
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::Text { .. }))
        })
        .cloned();
    let latest_verified = messages
        .iter()
        .rev()
        .find(|msg| {
            msg.role == "user"
                && msg.content.iter().any(|block| match block {
                    ContentBlock::ToolResult { content, .. } => {
                        content.contains("[verification replay]")
                    }
                    _ => false,
                })
        })
        .cloned();
    (latest_user, latest_verified)
}

/// Reset a [`ChatHistory`] to `{latest_user, latest_verified}` — the mid-loop
/// `VerifyAndReplan` transcript mutation (§E slice 3a).
///
/// Mirrors the transcript portion of `Engine::apply_verify_and_replan`
/// (below) but operates through the framework-core `ChatHistory` trait
/// (`push`/`clear`) since the executor only holds `&mut dyn ChatHistory`
/// during `run`, not `&mut Session`. `SessionChatHistory` delegates to
/// `session.messages`, so this mutates the host's transcript in place; the
/// model sees the reset on the next request within the same turn (the loop
/// `continue`s and rebuilds the request from `history.messages()`).
pub(crate) fn reset_history_to_latest_user_and_verified(history: &mut dyn ChatHistory) {
    let (latest_user, latest_verified) = latest_user_and_verified(history.messages());
    history.clear();
    if let Some(msg) = latest_user {
        history.push(msg);
    }
    if let Some(msg) = latest_verified {
        history.push(msg);
    }
}

/// Trim oldest messages off the transcript until the estimated input tokens
/// fit `target` — the `&mut dyn ChatHistory` form of
/// [`Engine::trim_oldest_messages_to_budget`] (whose body is a pure loop over
/// `self.session.messages`), so the mid-loop `TargetedContextRefresh` path
/// (§E slice 3c) can run the local-trim fallback through `ChatHistory` without
/// `&mut Session`.
///
/// Mirrors `run_compaction`'s Phase-1 clone → mutate → clear+repush pattern:
/// `ChatHistory::messages()` is `&[Message]` (immutable), so the messages are
/// cloned, the oldest peeled off in a loop (keeping at least
/// `MIN_RECENT_MESSAGES_TO_KEEP`), then the survivors are cleared+repushed.
/// Returns the number removed.
pub(crate) fn trim_oldest_messages_to_budget_history(
    history: &mut dyn ChatHistory,
    system: Option<&SystemPrompt>,
    target_input_budget: usize,
) -> usize {
    let mut messages = history.messages().to_vec();
    let mut removed = 0usize;
    while messages.len() > MIN_RECENT_MESSAGES_TO_KEEP
        && estimate_input_tokens_conservative(&messages, system) > target_input_budget
    {
        messages.remove(0);
        removed = removed.saturating_add(1);
    }
    if removed > 0 {
        history.clear();
        for m in messages {
            history.push(m);
        }
    }
    removed
}

/// A replayable tool-use candidate resolved from the transcript (§E slice 3b).
///
/// The `&[Message]` analog of a `TurnToolCall`: the executor holds `&mut dyn
/// ChatHistory` during `run` (not a `TurnContext`), so the mid-loop replay
/// selects its candidate by scanning the transcript rather than
/// `turn.tool_calls`.
pub(crate) struct ReplayCandidate {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) input: serde_json::Value,
    /// The original `ToolResult.content` for this `tool_use_id` — what the
    /// replay output is compared against.
    pub(crate) original_result: String,
}

/// Whether `tool_name` is a read-only, replayable tool — the free-fn form of
/// [`Engine::tool_is_replayable_read_only`] (whose body reads no `self`
/// state), so the mid-loop executor path can call it without `&Engine`
/// (§E slice 3b).
pub(crate) fn is_replayable_read_only(
    tool_name: &str,
    tool_registry: Option<&dyn ToolDispatcher>,
) -> bool {
    if tool_name == MULTI_TOOL_PARALLEL_NAME || tool_name == REQUEST_USER_INPUT_NAME {
        return false;
    }
    if McpPool::is_mcp_tool(tool_name) {
        return mcp_tool_is_read_only(tool_name);
    }
    tool_registry
        .and_then(|registry| registry.metadata(tool_name))
        .is_some_and(|metadata| metadata.is_read_only)
}

/// Select the most recent successful, read-only, replayable tool-use from the
/// transcript — the `&[Message]` analog of [`Engine::select_replay_candidate`]
/// (which scans `turn.tool_calls`) (§E slice 3b).
///
/// A "candidate" is an assistant `ToolUse` whose matching user `ToolResult`
/// (by `tool_use_id`) is non-error (`is_error != Some(true)`) and whose tool
/// `is_replayable_read_only`. By-design divergence from the legacy
/// `TurnToolCall`-based selection (which keys on `error.is_none() &&
/// result.is_some()`): the transcript only exposes `is_error`, which is
/// `Some(true)` for both dispatch-`Err` and `Ok(ToolResult { success: false })`,
/// so this path selects only fully-successful tools — replaying a successful
/// tool to verify idempotency is the replay's intent anyway.
pub(crate) fn select_replay_candidate_from_messages(
    messages: &[Message],
    tool_registry: Option<&dyn ToolDispatcher>,
) -> Option<ReplayCandidate> {
    // Map every `ToolResult` by `tool_use_id` → (content, is_error).
    use std::collections::HashMap;
    let mut results: HashMap<&str, (&str, Option<bool>)> = HashMap::new();
    for msg in messages {
        for block in &msg.content {
            if let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                ..
            } = block
            {
                results.insert(tool_use_id.as_str(), (content.as_str(), *is_error));
            }
        }
    }
    // Most-recent successful replayable `ToolUse`.
    for msg in messages.iter().rev() {
        if msg.role != "assistant" {
            continue;
        }
        for block in msg.content.iter().rev() {
            if let ContentBlock::ToolUse { id, name, input, .. } = block {
                if let Some((content, is_error)) = results.get(id.as_str()) {
                    if *is_error != Some(true)
                        && is_replayable_read_only(name, tool_registry)
                    {
                        return Some(ReplayCandidate {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                            original_result: (*content).to_string(),
                        });
                    }
                }
            }
        }
    }
    None
}

/// Re-execute the most recent successful read-only tool-use and push the
/// `[verification replay]` `ToolResult` note onto the transcript via
/// `ChatHistory` — the mid-loop `VerifyWithToolReplay` transcript mutation
/// (§E slice 3b).
///
/// Mirrors the transcript portion of [`Engine::apply_verify_with_tool_replay`]
/// (candidate select → re-execute → pass/fail → build note → push) but operates
/// through the framework-core `ChatHistory` trait (`push`) since the executor
/// only holds `&mut dyn ChatHistory` during `run`, not `&mut Session`. The
/// re-execution uses `tool_registry.execute` (the same dispatch surface the
/// legacy path uses inside `execute_tool_with_lock`, minus the `ToolExecGuard`
/// lock + `mcp_pool`). Returns the outcome so the host's post-`run` call can
/// run the state work (canonical persist, system-prompt fold, emit, mark) with
/// `skip_transcript = true`. Returns `None` when no candidate is found (the
/// host then no-ops). Does **not** call `mark_replay_failed` — the
/// `CapacityGateProbe` doesn't expose it, and it's state work (post-`run`).
pub(crate) async fn replay_and_push_verification_note(
    history: &mut dyn ChatHistory,
    tool_registry: Option<&dyn ToolDispatcher>,
) -> Option<ReplayOutcome> {
    let candidate = select_replay_candidate_from_messages(history.messages(), tool_registry)?;
    let registry = tool_registry?;

    let replay_result = registry
        .execute(&candidate.name, candidate.input.clone(), None)
        .await;

    let (pass, replay_outcome, diff_summary) = match replay_result {
        Ok(output) => {
            let original = candidate.original_result.as_str();
            let replay = output.content.as_str();
            let equal = original.trim() == replay.trim();
            let diff = if equal {
                "output_match".to_string()
            } else {
                format!(
                    "output_mismatch: original='{}' replay='{}'",
                    summarize_text(original, 140),
                    summarize_text(replay, 140)
                )
            };
            (
                equal,
                if equal {
                    "pass".to_string()
                } else {
                    "conflict".to_string()
                },
                diff,
            )
        }
        Err(err) => (
            false,
            "error".to_string(),
            format!("replay_error: {}", summarize_text(&err.to_string(), 180)),
        ),
    };

    let verification_note = format!(
        "[verification replay] tool={} pass={} details={}",
        candidate.name, pass, diff_summary
    );
    history.push(Message {
        role: "user".to_string(),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: candidate.id.clone(),
            content: verification_note.clone(),
            is_error: None,
            content_blocks: None,
        }],
    });

    Some(ReplayOutcome {
        tool_id: candidate.id,
        tool_name: candidate.name,
        pass,
        replay_outcome,
        diff_summary,
        verification_note,
    })
}

/// Capacity-controller (Gate A) probe for the executor path (§E slice 33).
///
/// Mirrors the established `CompactionProbe` / `CapacityProbe` /
/// `TurnMetaProbe` pattern: an `Option<CapacityGateProbe>` field on the
/// executor, constructed at executor-build time (before the `&mut self.session`
/// borrow held by `SessionChatHistory`) and carrying `Arc` clones of the
/// controller + working set so the executor can observe + decide mid-loop
/// (at seam 1 pre-request and seam 4 post-tool) without needing `&mut self`
/// on the `Engine`.
///
/// The executor observes + decides mid-loop and signals via a one-shot slot
/// (`pending_capacity_decision`); the host applies the full `impl Engine`
/// intervention cascade post-`run` (where `&mut self.session` is back in host
/// hands). Deferring application to post-`run` is behavior-equivalent because
/// the executor's system prompt is a static snapshot (`let system =
/// self.config.system.clone()`), so any system-prompt change is invisible to
/// the same turn's requests — the intervention takes effect on the next turn.
pub struct CapacityGateProbe {
    controller: Arc<std::sync::Mutex<CapacityController>>,
    model: String,
    /// Workspace root for the mid-loop `TargetedContextRefresh` transcript
    /// portion (§E slice 3c): `pinned_message_indices(messages, workspace)` +
    /// `should_compact(.., Some(workspace), ..)`. Mirrors
    /// `self.session.workspace`.
    workspace: PathBuf,
    working_set: Arc<std::sync::Mutex<WorkingSet>>,
    profile_window: usize,
    turn_index: u64,
}

impl CapacityGateProbe {
    /// Construct from `Arc` clones of the controller + working set, the
    /// session model / workspace (snapshots, immutable for a turn), the
    /// capacity profile-window (from `config.capacity.profile_window`), and
    /// the turn index (from `Engine.turn_counter`, captured at construction).
    #[must_use]
    pub fn new(
        controller: Arc<std::sync::Mutex<CapacityController>>,
        model: String,
        workspace: PathBuf,
        working_set: Arc<std::sync::Mutex<WorkingSet>>,
        profile_window: usize,
        turn_index: u64,
    ) -> Self {
        Self {
            controller,
            model,
            workspace,
            working_set,
            profile_window,
            turn_index,
        }
    }

    /// Borrow the shared working set (§E slice 3c). The mid-loop
    /// `TargetedContextRefresh` transcript portion needs the working set to
    /// build compaction pins (`pinned_message_indices`) + paths (`top_paths`),
    /// mirroring `Engine::apply_targeted_context_refresh`'s
    /// `self.session.working_set` reads. The working set is `Arc`-shared with
    /// the session's, so reads here see live state.
    pub(crate) fn working_set(&self) -> &Arc<std::sync::Mutex<WorkingSet>> {
        &self.working_set
    }

    /// Borrow the workspace root (§E slice 3c). Used by the mid-loop
    /// `TargetedContextRefresh` for `pinned_message_indices(messages, workspace)`
    /// + `should_compact(.., Some(workspace), ..)`, mirroring
    /// `Engine::apply_targeted_context_refresh`'s `self.session.workspace`.
    pub(crate) fn workspace(&self) -> &Path {
        self.workspace.as_path()
    }

    /// Build `CapacityObservationInput` from the executor's message view,
    /// the current step, recent tool-call IDs, and the system prompt snapshot.
    /// Faithful to `Engine::capacity_observation` — uses the same free functions
    /// (`recent_tool_call_count`, `recent_unique_reference_count`) and the same
    /// token-estimation / context-window logic.
    fn build_observation(
        &self,
        messages: &[Message],
        step: u32,
        tool_call_ids: &[String],
        system: Option<&SystemPrompt>,
    ) -> CapacityObservationInput {
        let message_window = self.profile_window.max(8) * 3;
        let action_count_this_turn = usize::try_from(step)
            .unwrap_or(usize::MAX)
            .saturating_add(tool_call_ids.len())
            .saturating_add(1);
        let tool_calls_recent_window = recent_tool_call_count(messages, message_window);
        let working_set = self
            .working_set
            .lock()
            .expect("working_set poisoned");
        let unique_reference_ids_recent_window = recent_unique_reference_count(
            messages,
            message_window,
            tool_call_ids,
            &working_set,
        );
        let context_window = usize::try_from(
            context_window_for_model(&self.model)
                .unwrap_or(LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS),
        )
        .unwrap_or(usize::try_from(LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS).unwrap_or(128_000))
        .max(1);
        let context_used_ratio =
            (estimate_input_tokens_conservative(messages, system) as f64) / (context_window as f64);

        CapacityObservationInput {
            turn_index: self.turn_index,
            model: self.model.clone(),
            action_count_this_turn,
            tool_calls_recent_window,
            unique_reference_ids_recent_window,
            context_used_ratio,
        }
    }

    /// Observe pre-turn (seam 1). Returns `None` if the controller is disabled
    /// (the controller's `observe` returns `None` when `config.enabled` is false).
    pub fn observe_pre_turn(
        &self,
        messages: &[Message],
        step: u32,
        tool_call_ids: &[String],
        system: Option<&SystemPrompt>,
    ) -> Option<CapacitySnapshot> {
        let input = self.build_observation(messages, step, tool_call_ids, system);
        self.controller
            .lock()
            .expect("capacity_controller poisoned")
            .observe_pre_turn(input)
    }

    /// Observe post-tool (seam 4). Returns `None` if the controller is disabled.
    pub fn observe_post_tool(
        &self,
        messages: &[Message],
        step: u32,
        tool_call_ids: &[String],
        system: Option<&SystemPrompt>,
    ) -> Option<CapacitySnapshot> {
        let input = self.build_observation(messages, step, tool_call_ids, system);
        self.controller
            .lock()
            .expect("capacity_controller poisoned")
            .observe_post_tool(input)
    }

    /// Decide intervention from the latest snapshot, with cooldown and safety gates.
    pub fn decide(&self, snapshot: Option<&CapacitySnapshot>) -> CapacityDecision {
        self.controller
            .lock()
            .expect("capacity_controller poisoned")
            .decide(self.turn_index, snapshot)
    }

    /// Mark an intervention as applied for this turn (prevents double-intervention
    /// — seam 4's `decide` will see the cooldown and return `NoIntervention`).
    pub fn mark_intervention_applied(&self, action: GuardrailAction) {
        self.controller
            .lock()
            .expect("capacity_controller poisoned")
            .mark_intervention_applied(self.turn_index, action);
    }

    /// Last observed snapshot (clone before releasing lock).
    pub fn last_snapshot(&self) -> Option<CapacitySnapshot> {
        self.controller
            .lock()
            .expect("capacity_controller poisoned")
            .last_snapshot()
            .cloned()
    }

    /// Error-escalation checkpoint (sub-slice 2 of Gate A, slice 34 §E).
    /// Mirrors `Engine::run_capacity_error_escalation_checkpoint`'s
    /// observe/force/decide logic. The executor tracks the per-step error
    /// counts + categories; this method does the capacity-side
    /// observe/force/decide, returning `Some(decision)` only when the
    /// controller decides `VerifyAndReplan` (production lines 386–388).
    ///
    /// The controller's per-turn cooldown (`intervention_applied_turn`, set
    /// by seam 1/4's `mark_intervention_applied`) naturally blocks this when
    /// an earlier checkpoint already intervened — `decide` returns
    /// `NoIntervention` at the cooldown check (capacity.rs:228) before
    /// reaching `decide_policy`, mirroring production's "seam 4 fires →
    /// `continue` → error-escalation skipped". No explicit guard is needed.
    ///
    /// The decision reason is overridden with the escalation format
    /// (production lines 390–401) so the host post-`run`
    /// `apply_verify_and_replan` records it via slice 33's
    /// `&decision.reason` plumbing.
    pub fn decide_error_escalation(
        &self,
        messages: &[Message],
        step: u32,
        tool_call_ids: &[String],
        system: Option<&SystemPrompt>,
        step_error_count: usize,
        consecutive_tool_error_steps: u32,
        error_categories: &[ErrorCategory],
    ) -> Option<CapacityDecision> {
        // Early-return gating (production lines 332–353): no errors at all;
        // transient-only without context overflow; non-overflow with fewer
        // than 2 consecutive error steps.
        if step_error_count == 0 && consecutive_tool_error_steps < 2 {
            return None;
        }
        // Categorize this step's failures by typed `ErrorCategory` rather than
        // substring-matching error strings. Context overflow always escalates;
        // network / rate-limit / timeout are transient and skip escalation;
        // anything else only escalates with consecutive failures.
        let has_context_overflow = error_categories.contains(&ErrorCategory::InvalidInput);
        let only_transient = !error_categories.is_empty()
            && error_categories.iter().all(|c| {
                matches!(
                    c,
                    ErrorCategory::Network | ErrorCategory::RateLimit | ErrorCategory::Timeout
                )
            });
        if only_transient && !has_context_overflow {
            return None;
        }
        if !has_context_overflow && consecutive_tool_error_steps < 2 {
            return None;
        }

        // Get/observe snapshot (production lines 355–369). `last_snapshot()`
        // clones + releases the lock before `or_else`, so there is no
        // re-entrant deadlock (the slice 33 deadlock-fix concern does not
        // apply to the probe path).
        let last = self.last_snapshot();
        let snapshot = last.or_else(|| self.observe_pre_turn(messages, step, tool_call_ids, system));
        let Some(snapshot) = snapshot else {
            return None;
        };

        // Force to High+severe if repeated failures (production lines 371–376).
        let repeated_failures = step_error_count >= 2 || consecutive_tool_error_steps >= 2;
        let mut forced = snapshot.clone();
        if repeated_failures && !(snapshot.risk_band == RiskBand::High && snapshot.severe) {
            forced.risk_band = RiskBand::High;
            forced.severe = true;
        }

        // Decide (production lines 378–382). The controller's per-turn
        // cooldown (set by seam 1/4) returns `NoIntervention` here if an
        // earlier checkpoint already intervened.
        let mut decision = self.decide(Some(&forced));

        // Only act on VerifyAndReplan (production lines 386–388).
        if decision.action != GuardrailAction::VerifyAndReplan {
            return None;
        }

        // Override the reason with the escalation format (production lines
        // 390–401) so the host post-`run` `apply_verify_and_replan` records
        // the escalation context (slice 33 passes `&decision.reason`).
        let category_labels: Vec<String> =
            error_categories.iter().map(|c| c.to_string()).collect();
        decision.reason = format!(
            "error_escalation: step_errors={}, consecutive_steps={}, categories={}",
            step_error_count,
            consecutive_tool_error_steps,
            category_labels.join(",")
        );
        Some(decision)
    }
}

impl Engine {
    pub async fn run_capacity_pre_request_checkpoint(
        &mut self,
        turn: &TurnContext,
        client: Option<&dyn crate::llm_client::LlmClient>,
        mode: AppMode,
    ) -> bool {
        let snapshot = self
            .capacity_controller
            .lock()
            .expect("capacity_controller poisoned")
            .observe_pre_turn(self.capacity_observation(turn));
        let decision = self
            .capacity_controller
            .lock()
            .expect("capacity_controller poisoned")
            .decide(self.turn_counter, snapshot.as_ref());
        self.emit_capacity_decision(turn, snapshot.as_ref(), &decision)
            .await;

        if decision.action != GuardrailAction::TargetedContextRefresh {
            return false;
        }

        self.apply_targeted_context_refresh(turn, client, mode, snapshot.as_ref(), false, None)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run_capacity_post_tool_checkpoint(
        &mut self,
        turn: &TurnContext,
        mode: AppMode,
        tool_registry: Option<&dyn ToolDispatcher>,
        tool_exec_lock: Arc<RwLock<()>>,
        mcp_pool: Option<Arc<AsyncMutex<McpPool>>>,
        _step_error_count: usize,
        _consecutive_tool_error_steps: u32,
    ) -> bool {
        let snapshot = self
            .capacity_controller
            .lock()
            .expect("capacity_controller poisoned")
            .observe_post_tool(self.capacity_observation(turn));
        let decision = self
            .capacity_controller
            .lock()
            .expect("capacity_controller poisoned")
            .decide(self.turn_counter, snapshot.as_ref());
        self.emit_capacity_decision(turn, snapshot.as_ref(), &decision)
            .await;

        match decision.action {
            GuardrailAction::VerifyWithToolReplay => {
                let _ = self
                    .apply_verify_with_tool_replay(
                        turn,
                        mode,
                        snapshot.as_ref(),
                        tool_registry,
                        tool_exec_lock,
                        mcp_pool,
                        false,
                        None,
                    )
                    .await;
                false
            }
            GuardrailAction::VerifyAndReplan => {
                self.apply_verify_and_replan(turn, mode, snapshot.as_ref(), "high_risk_post_tool", false)
                    .await
            }
            GuardrailAction::NoIntervention | GuardrailAction::TargetedContextRefresh => false,
        }
    }

    pub async fn run_capacity_error_escalation_checkpoint(
        &mut self,
        turn: &TurnContext,
        mode: AppMode,
        step_error_count: usize,
        consecutive_tool_error_steps: u32,
        error_categories: &[ErrorCategory],
    ) -> bool {
        if step_error_count == 0 && consecutive_tool_error_steps < 2 {
            return false;
        }

        // Categorize this step's failures by typed `ErrorCategory` rather than
        // substring-matching error strings. Context overflow always escalates;
        // network / rate-limit / timeout are transient and skip escalation;
        // anything else only escalates with consecutive consecutive failures.
        let has_context_overflow = error_categories.contains(&ErrorCategory::InvalidInput);
        let only_transient = !error_categories.is_empty()
            && error_categories.iter().all(|c| {
                matches!(
                    c,
                    ErrorCategory::Network | ErrorCategory::RateLimit | ErrorCategory::Timeout
                )
            });
        if only_transient && !has_context_overflow {
            return false;
        }
        if !has_context_overflow && consecutive_tool_error_steps < 2 {
            return false;
        }

        let last = self
            .capacity_controller
            .lock()
            .expect("capacity_controller poisoned")
            .last_snapshot()
            .cloned();
        let snapshot = last.or_else(|| {
            self.capacity_controller
                .lock()
                .expect("capacity_controller poisoned")
                .observe_pre_turn(self.capacity_observation(turn))
        });
        let Some(snapshot) = snapshot else {
            return false;
        };

        let repeated_failures = step_error_count >= 2 || consecutive_tool_error_steps >= 2;
        let mut forced = snapshot.clone();
        if repeated_failures && !(snapshot.risk_band == RiskBand::High && snapshot.severe) {
            forced.risk_band = RiskBand::High;
            forced.severe = true;
        }

        let decision = self
            .capacity_controller
            .lock()
            .expect("capacity_controller poisoned")
            .decide(self.turn_counter, Some(&forced));
        self.emit_capacity_decision(turn, Some(&forced), &decision)
            .await;

        if decision.action != GuardrailAction::VerifyAndReplan {
            return false;
        }

        let category_labels: Vec<String> = error_categories.iter().map(|c| c.to_string()).collect();
        self.apply_verify_and_replan(
            turn,
            mode,
            Some(&forced),
            &format!(
                "error_escalation: step_errors={}, consecutive_steps={}, categories={}",
                step_error_count,
                consecutive_tool_error_steps,
                category_labels.join(",")
            ),
            false,
        )
        .await
    }

    pub fn capacity_observation(&self, turn: &TurnContext) -> CapacityObservationInput {
        let message_window = self.config.capacity.profile_window.max(8) * 3;
        let action_count_this_turn = usize::try_from(turn.step)
            .unwrap_or(usize::MAX)
            .saturating_add(turn.tool_calls.len())
            .saturating_add(1);
        let tool_calls_recent_window = self.recent_tool_call_count(message_window);
        let unique_reference_ids_recent_window =
            self.recent_unique_reference_count(message_window, turn);
        let context_window = usize::try_from(
            context_window_for_model(&self.session.model)
                .unwrap_or(LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS),
        )
        .unwrap_or(usize::try_from(LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS).unwrap_or(128_000))
        .max(1);
        let context_used_ratio = (self.estimated_input_tokens() as f64) / (context_window as f64);

        CapacityObservationInput {
            turn_index: self.turn_counter,
            model: self.session.model.clone(),
            action_count_this_turn,
            tool_calls_recent_window,
            unique_reference_ids_recent_window,
            context_used_ratio,
        }
    }

    pub fn recent_tool_call_count(&self, message_window: usize) -> usize {
        recent_tool_call_count(&self.session.messages, message_window)
    }

    pub fn recent_unique_reference_count(
        &self,
        message_window: usize,
        turn: &TurnContext,
    ) -> usize {
        let working_set = self
            .session
            .working_set
            .lock()
            .expect("working_set poisoned");
        let ids: Vec<String> = turn.tool_calls.iter().map(|t| t.id.clone()).collect();
        recent_unique_reference_count(
            &self.session.messages,
            message_window,
            &ids,
            &working_set,
        )
    }

    pub async fn emit_coherence_signal(
        &mut self,
        signal: CoherenceSignal,
        reason: impl Into<String>,
    ) {
        let next = next_coherence_state(self.coherence_state, signal);
        self.coherence_state = next;
        let _ = self
            .tx_event
            .send(Event::CoherenceState {
                state: next,
                label: next.label().to_string(),
                description: next.description().to_string(),
                reason: reason.into(),
            })
            .await;
    }

    pub async fn emit_compaction_started(&mut self, id: String, auto: bool, message: String) {
        let _ = self
            .tx_event
            .send(Event::CompactionStarted {
                id,
                auto,
                message: message.clone(),
            })
            .await;
        self.emit_coherence_signal(CoherenceSignal::CompactionStarted, message)
            .await;
    }

    pub async fn emit_compaction_completed(
        &mut self,
        id: String,
        auto: bool,
        message: String,
        messages_before: Option<usize>,
        messages_after: Option<usize>,
    ) {
        let _ = self
            .tx_event
            .send(Event::CompactionCompleted {
                id,
                auto,
                message: message.clone(),
                messages_before,
                messages_after,
            })
            .await;
        self.emit_coherence_signal(CoherenceSignal::CompactionCompleted, message)
            .await;
    }

    pub async fn emit_compaction_failed(&mut self, id: String, auto: bool, message: String) {
        let _ = self
            .tx_event
            .send(Event::CompactionFailed {
                id,
                auto,
                message: message.clone(),
            })
            .await;
        self.emit_coherence_signal(CoherenceSignal::CompactionFailed, message)
            .await;
    }

    /// Mirror a capacity [`Event`] into the local telemetry sink (Plan 06/6.1).
    ///
    /// Builds a `serde_json::Value` from the three capacity event variants and
    /// hands it to [`TelemetrySink::emit`] when `config.telemetry_sink` is
    /// `Some`. Non-capacity events are ignored. Potentially-leaky string
    /// fields (`replay_outcome`, `error`) arrive as `RedactedAnalyticsMetadata`
    /// (Plan 06 / 6.3) and are emitted via `.as_str()`, so only the sanitized
    /// values reach the sink. IO failures are swallowed by the sink —
    /// telemetry never breaks the engine. The same `Event` is still sent on
    /// `tx_event` for the UI, so this routing is purely additive.
    fn emit_telemetry(&self, event: &Event) {
        let Some(sink) = self.config.telemetry_sink.as_ref() else {
            return;
        };
        let value = match event {
            Event::CapacityDecision {
                session_id,
                turn_id,
                h_hat,
                c_hat,
                slack,
                min_slack,
                violation_ratio,
                p_fail,
                risk_band,
                action,
                cooldown_blocked,
                reason,
            } => serde_json::json!({
                "type": "capacity_decision",
                "session_id": session_id.as_str(),
                "turn_id": turn_id.as_str(),
                "h_hat": h_hat,
                "c_hat": c_hat,
                "slack": slack,
                "min_slack": min_slack,
                "violation_ratio": violation_ratio,
                "p_fail": p_fail,
                "risk_band": risk_band.as_str(),
                "action": action.as_str(),
                "cooldown_blocked": cooldown_blocked,
                "reason": reason.as_str(),
            }),
            Event::CapacityIntervention {
                session_id,
                turn_id,
                action,
                before_prompt_tokens,
                after_prompt_tokens,
                compaction_size_reduction,
                replay_outcome,
                replan_performed,
            } => serde_json::json!({
                "type": "capacity_intervention",
                "session_id": session_id.as_str(),
                "turn_id": turn_id.as_str(),
                "action": action.as_str(),
                "before_prompt_tokens": before_prompt_tokens,
                "after_prompt_tokens": after_prompt_tokens,
                "compaction_size_reduction": compaction_size_reduction,
                "replay_outcome": replay_outcome.as_ref().map(|r| r.as_str()),
                "replan_performed": replan_performed,
            }),
            Event::CapacityMemoryPersistFailed {
                session_id,
                turn_id,
                action,
                error,
            } => serde_json::json!({
                "type": "capacity_memory_persist_failed",
                "session_id": session_id.as_str(),
                "turn_id": turn_id.as_str(),
                "action": action.as_str(),
                "error": error.as_str(),
            }),
            _ => return,
        };
        sink.emit(value);
    }

    pub async fn emit_capacity_decision(
        &mut self,
        turn: &TurnContext,
        snapshot: Option<&CapacitySnapshot>,
        decision: &CapacityDecision,
    ) {
        let Some(snapshot) = snapshot else {
            return;
        };
        let event = Event::CapacityDecision {
            session_id: VerifiedAnalyticsMetadata::verified(&self.session.telemetry_session_id),
            turn_id: VerifiedAnalyticsMetadata::verified(&turn.id),
            h_hat: snapshot.h_hat,
            c_hat: snapshot.c_hat,
            slack: snapshot.slack,
            min_slack: snapshot.profile.min_slack,
            violation_ratio: snapshot.profile.violation_ratio,
            p_fail: snapshot.p_fail,
            risk_band: VerifiedAnalyticsMetadata::verified(snapshot.risk_band.as_str()),
            action: VerifiedAnalyticsMetadata::verified(decision.action.as_str()),
            cooldown_blocked: decision.cooldown_blocked,
            reason: VerifiedAnalyticsMetadata::verified(&decision.reason),
        };
        self.emit_telemetry(&event);
        let _ = self.tx_event.send(event).await;
        self.emit_coherence_signal(
            CoherenceSignal::CapacityDecision {
                risk_band: snapshot.risk_band,
                action: decision.action,
                cooldown_blocked: decision.cooldown_blocked,
            },
            format!(
                "capacity_decision: risk={} action={} reason={}",
                snapshot.risk_band.as_str(),
                decision.action.as_str(),
                decision.reason
            ),
        )
        .await;
    }

    pub async fn emit_capacity_intervention(
        &mut self,
        turn: &TurnContext,
        action: GuardrailAction,
        before_prompt_tokens: usize,
        after_prompt_tokens: usize,
        replay_outcome: Option<String>,
        replan_performed: bool,
    ) {
        let event = Event::CapacityIntervention {
            session_id: VerifiedAnalyticsMetadata::verified(&self.session.telemetry_session_id),
            turn_id: VerifiedAnalyticsMetadata::verified(&turn.id),
            action: VerifiedAnalyticsMetadata::verified(action.as_str()),
            before_prompt_tokens,
            after_prompt_tokens,
            compaction_size_reduction: before_prompt_tokens.saturating_sub(after_prompt_tokens),
            replay_outcome: replay_outcome.map(|s| RedactedAnalyticsMetadata::redact(&s)),
            replan_performed,
        };
        self.emit_telemetry(&event);
        let _ = self.tx_event.send(event).await;
        self.emit_coherence_signal(
            CoherenceSignal::CapacityIntervention { action },
            format!("capacity_intervention: action={}", action.as_str()),
        )
        .await;
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn apply_targeted_context_refresh(
        &mut self,
        turn: &TurnContext,
        client: Option<&dyn crate::llm_client::LlmClient>,
        mode: AppMode,
        snapshot: Option<&CapacitySnapshot>,
        skip_transcript: bool,
        outcome: Option<TargetedRefreshOutcome>,
    ) -> bool {
        // §E slice 3c: when the executor already applied the transcript portion
        // (LLM compaction + reinject + local-trim fallback) mid-loop at seam-1
        // via `ChatHistory`, `skip_transcript = true` skips re-doing it here —
        // re-compacting/re-pushing would double-mutate the transcript. The
        // carried `outcome` supplies `before_tokens` (captured before the
        // mid-loop refresh) + `refreshed` (whether the transcript was actually
        // reduced). The host passes `skip_transcript = outcome.is_some()`, so a
        // mid-loop refresh that ran (seam-1) arrives as `Some(outcome)` and runs
        // only state work; a `TargetedContextRefresh` that fell through at
        // seam-4 (no mid-loop compaction) arrives as `outcome == None` with
        // `skip_transcript = false` and runs the full cascade below. The
        // `Some(outcome) else { return false }` guard mirrors 3b's defensive
        // early-return for a `skip_transcript = true, None` mis-call.
        let (before_tokens, refreshed) = if skip_transcript {
            let Some(outcome) = outcome else {
                return false;
            };
            (outcome.before_tokens, outcome.refreshed)
        } else {
            // === transcript portion (legacy post-`run` path; dead-code / test
            // callers via `run_capacity_pre_request_checkpoint`) ===
            let before_tokens = self.estimated_input_tokens();
            let compaction_pins = self
                .session
                .working_set
                .lock()
                .expect("working_set poisoned")
                .pinned_message_indices(&self.session.messages, &self.session.workspace);
            let compaction_paths = self
                .session
                .working_set
                .lock()
                .expect("working_set poisoned")
                .top_paths(24);

            let mut refreshed = false;
            let should_run_summary_compaction = self.config.compaction.enabled
                && should_compact(
                    &self.session.messages,
                    &self.config.compaction,
                    Some(&self.session.workspace),
                    Some(&compaction_pins),
                    Some(&compaction_paths),
                );
            if should_run_summary_compaction && let Some(client) = client {
                let enhancements = self.build_compaction_enhancements();
                match compact_messages_safe(
                    client,
                    &self.session.messages,
                    &self.config.compaction,
                    Some(&self.session.workspace),
                    Some(&compaction_pins),
                    Some(&compaction_paths),
                    enhancements.as_ref(),
                )
                .await
                {
                    Ok(result) => {
                        if !result.messages.is_empty() || self.session.messages.is_empty() {
                            self.session.messages = result.messages;
                            self.merge_compaction_summary(result.summary_prompt);
                            self.reinject_compaction_attachments(context_input_budget_for_provider(
                                self.api_provider,
                                &self.session.model,
                            ))
                            .await;
                            refreshed = true;
                        }
                    }
                    Err(err) => {
                        let _ = self
                            .tx_event
                            .send(Event::status(format!(
                                "Capacity refresh compaction failed: {err}. Falling back to local trim."
                            )))
                            .await;
                    }
                }
            }

            if !refreshed {
                let target_budget =
                    context_input_budget_for_provider(self.api_provider, &self.session.model)
                        .unwrap_or(self.config.compaction.token_threshold.max(1));
                if self.estimated_input_tokens() > target_budget {
                    let trimmed = self.trim_oldest_messages_to_budget(target_budget);
                    refreshed = trimmed > 0;
                    if refreshed {
                        self.reinject_compaction_attachments(Some(target_budget))
                            .await;
                    }
                }
            }
            (before_tokens, refreshed)
        };

        if !refreshed {
            return false;
        }

        let canonical = self.build_canonical_state(turn, None);
        let source_message_ids = self.capacity_source_message_ids(turn);
        let record = self.build_capacity_record(
            turn,
            GuardrailAction::TargetedContextRefresh,
            snapshot,
            canonical.clone(),
            source_message_ids,
            None,
        );
        let pointer = self
            .persist_capacity_record(turn, GuardrailAction::TargetedContextRefresh, &record)
            .await;
        self.merge_compaction_summary(Some(self.canonical_prompt(
            &canonical,
            &pointer,
            GuardrailAction::TargetedContextRefresh,
            None,
        )));
        self.refresh_system_prompt(mode);
        self.emit_session_updated().await;

        let after_tokens = self.estimated_input_tokens();
        self.emit_capacity_intervention(
            turn,
            GuardrailAction::TargetedContextRefresh,
            before_tokens,
            after_tokens,
            None,
            false,
        )
        .await;
        self.capacity_controller
            .lock()
            .expect("capacity_controller poisoned")
            .mark_intervention_applied(self.turn_counter, GuardrailAction::TargetedContextRefresh);
        true
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn apply_verify_with_tool_replay(
        &mut self,
        turn: &TurnContext,
        mode: AppMode,
        snapshot: Option<&CapacitySnapshot>,
        tool_registry: Option<&dyn ToolDispatcher>,
        tool_exec_lock: Arc<RwLock<()>>,
        mut mcp_pool: Option<Arc<AsyncMutex<McpPool>>>,
        skip_transcript: bool,
        outcome: Option<ReplayOutcome>,
    ) -> bool {
        let before_tokens = self.estimated_input_tokens();

        // §E slice 3b: when the executor already applied the transcript portion
        // (candidate select → re-execute → push `[verification replay]` note)
        // mid-loop via `ChatHistory`, `skip_transcript = true` skips re-doing it
        // here — re-executing + pushing again would double-inject the note. The
        // carried `outcome` supplies the values the state work below needs
        // (canonical note, `ReplayInfo`, `verification_note`, emit label).
        // `outcome == None` means the mid-loop replay found no candidate →
        // no-op (mirrors the legacy `select_replay_candidate` returning `None`).
        let (candidate_id, candidate_name, pass, replay_outcome, diff_summary, verification_note) =
            if skip_transcript {
                let Some(outcome) = outcome else {
                    return false;
                };
                (
                    outcome.tool_id,
                    outcome.tool_name,
                    outcome.pass,
                    outcome.replay_outcome,
                    outcome.diff_summary,
                    outcome.verification_note,
                )
            } else {
                // === transcript portion (legacy post-`run` path, dead-code /
                // test callers) ===
                let Some(candidate) = self.select_replay_candidate(turn, tool_registry) else {
                    return false;
                };

                if McpPool::is_mcp_tool(&candidate.name) && mcp_pool.is_none() {
                    mcp_pool = self.ensure_mcp_pool().await.ok();
                }

                let supports_parallel = if McpPool::is_mcp_tool(&candidate.name) {
                    mcp_tool_is_parallel_safe(&candidate.name)
                } else {
                    tool_registry
                        .and_then(|registry| registry.metadata(&candidate.name))
                        .is_some_and(|metadata| metadata.supports_parallel)
                };
                let interactive = if McpPool::is_mcp_tool(&candidate.name) {
                    false
                } else {
                    tool_registry
                        .is_some_and(|registry| registry.is_interactive(&candidate.name, &candidate.input))
                };

                let replay_result = Self::execute_tool_with_lock(
                    tool_exec_lock,
                    supports_parallel,
                    interactive,
                    self.tx_event.clone(),
                    candidate.name.clone(),
                    candidate.input.clone(),
                    tool_registry,
                    mcp_pool.clone(),
                    None,
                )
                .await;

                let (pass, replay_outcome, diff_summary) = match replay_result {
                    Ok(output) => {
                        let original = candidate.result.as_deref().unwrap_or_default();
                        let replay = output.content.as_str();
                        let equal = original.trim() == replay.trim();
                        let diff = if equal {
                            "output_match".to_string()
                        } else {
                            format!(
                                "output_mismatch: original='{}' replay='{}'",
                                summarize_text(original, 140),
                                summarize_text(replay, 140)
                            )
                        };
                        (
                            equal,
                            if equal {
                                "pass".to_string()
                            } else {
                                "conflict".to_string()
                            },
                            diff,
                        )
                    }
                    Err(err) => {
                        self.capacity_controller
                            .lock()
                            .expect("capacity_controller poisoned")
                            .mark_replay_failed(self.turn_counter);
                        (
                            false,
                            "error".to_string(),
                            format!("replay_error: {}", summarize_text(&err.to_string(), 180)),
                        )
                    }
                };

                let verification_note = format!(
                    "[verification replay] tool={} pass={} details={}",
                    candidate.name, pass, diff_summary
                );
                self.add_session_message(Message {
                    role: "user".to_string(),
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: candidate.id.clone(),
                        content: verification_note.clone(),
                        is_error: None,
                        content_blocks: None,
                    }],
                })
                .await;

                (candidate.id, candidate.name, pass, replay_outcome, diff_summary, verification_note)
            };

        // State work — always runs (canonical persist, system-prompt fold, emit,
        // mark). `mark_replay_failed` on `!pass` covers both branches (the legacy
        // transcript path fired it in the `Err` arm above for dispatch errors;
        // the `skip_transcript` path has no mid-loop `mark_replay_failed` since
        // `CapacityGateProbe` doesn't expose it).
        if !pass {
            self.capacity_controller
                .lock()
                .expect("capacity_controller poisoned")
                .mark_replay_failed(self.turn_counter);
        }

        let canonical = self.build_canonical_state(
            turn,
            Some(if pass {
                "replay verification passed"
            } else {
                "replay verification failed or conflicted"
            }),
        );
        let replay_info = Some(ReplayInfo {
            tool_id: candidate_id.clone(),
            tool_name: candidate_name.clone(),
            pass,
            diff_summary: diff_summary.clone(),
        });
        let source_message_ids = self.capacity_source_message_ids(turn);
        let record = self.build_capacity_record(
            turn,
            GuardrailAction::VerifyWithToolReplay,
            snapshot,
            canonical.clone(),
            source_message_ids,
            replay_info,
        );
        let pointer = self
            .persist_capacity_record(turn, GuardrailAction::VerifyWithToolReplay, &record)
            .await;
        self.merge_compaction_summary(Some(self.canonical_prompt(
            &canonical,
            &pointer,
            GuardrailAction::VerifyWithToolReplay,
            Some(&verification_note),
        )));
        self.refresh_system_prompt(mode);
        self.emit_session_updated().await;

        let after_tokens = self.estimated_input_tokens();
        self.emit_capacity_intervention(
            turn,
            GuardrailAction::VerifyWithToolReplay,
            before_tokens,
            after_tokens,
            Some(replay_outcome),
            false,
        )
        .await;
        self.capacity_controller
            .lock()
            .expect("capacity_controller poisoned")
            .mark_intervention_applied(self.turn_counter, GuardrailAction::VerifyWithToolReplay);
        true
    }

    pub async fn apply_verify_and_replan(
        &mut self,
        turn: &TurnContext,
        mode: AppMode,
        snapshot: Option<&CapacitySnapshot>,
        reason: &str,
        skip_transcript: bool,
    ) -> bool {
        let before_tokens = self.estimated_input_tokens();
        let canonical = self.build_canonical_state(turn, Some(reason));
        let source_message_ids = self.capacity_source_message_ids(turn);
        let record = self.build_capacity_record(
            turn,
            GuardrailAction::VerifyAndReplan,
            snapshot,
            canonical.clone(),
            source_message_ids,
            None,
        );
        let pointer = self
            .persist_capacity_record(turn, GuardrailAction::VerifyAndReplan, &record)
            .await;

        // Transcript reset to `{latest_user, latest_verified}`. Skipped when the
        // executor already applied it mid-loop via `ChatHistory` (§E slice 3a)
        // — re-running it post-`run` would wipe the model's post-reset
        // replanning work. The state work below (canonical persist, system-prompt
        // fold, emit, mark) always runs.
        if !skip_transcript {
            let (latest_user, latest_verified) =
                latest_user_and_verified(&self.session.messages);
            self.session.messages.clear();
            if let Some(msg) = latest_user {
                self.session.messages.push(msg);
            }
            if let Some(msg) = latest_verified {
                self.session.messages.push(msg);
            }
        }

        self.merge_compaction_summary(Some(self.canonical_prompt(
            &canonical,
            &pointer,
            GuardrailAction::VerifyAndReplan,
            Some("Replan now from canonical state. Keep steps minimal and verifiable."),
        )));
        self.refresh_system_prompt(mode);
        self.emit_session_updated().await;

        let _ = self
            .tx_event
            .send(Event::status(
                "Capacity guardrail: context reset to canonical state; replanning step."
                    .to_string(),
            ))
            .await;

        let after_tokens = self.estimated_input_tokens();
        self.emit_capacity_intervention(
            turn,
            GuardrailAction::VerifyAndReplan,
            before_tokens,
            after_tokens,
            None,
            true,
        )
        .await;
        self.capacity_controller
            .lock()
            .expect("capacity_controller poisoned")
            .mark_intervention_applied(self.turn_counter, GuardrailAction::VerifyAndReplan);
        true
    }

    pub fn select_replay_candidate(
        &self,
        turn: &TurnContext,
        tool_registry: Option<&dyn ToolDispatcher>,
    ) -> Option<TurnToolCall> {
        turn.tool_calls
            .iter()
            .rev()
            .find(|call| {
                call.error.is_none()
                    && call.result.is_some()
                    && self.tool_is_replayable_read_only(&call.name, tool_registry)
            })
            .cloned()
    }

    pub fn tool_is_replayable_read_only(
        &self,
        tool_name: &str,
        tool_registry: Option<&dyn ToolDispatcher>,
    ) -> bool {
        // Delegates to the free fn (§E slice 3b) so the mid-loop executor path
        // shares one read-only check source; the method body reads no `self`.
        is_replayable_read_only(tool_name, tool_registry)
    }

    pub fn build_canonical_state(&self, turn: &TurnContext, note: Option<&str>) -> CanonicalState {
        let goal = self
            .session
            .messages
            .iter()
            .rev()
            .find_map(|msg| {
                if msg.role != "user" {
                    return None;
                }
                msg.content.iter().find_map(|block| match block {
                    ContentBlock::Text { text, .. } => Some(summarize_text(text, 220)),
                    _ => None,
                })
            })
            .unwrap_or_else(|| "Continue current task from compact state".to_string());

        let mut constraints = vec![
            format!("model={}", self.session.model),
            format!("workspace={}", self.session.workspace.display()),
        ];
        if let Some(note) = note {
            constraints.push(summarize_text(note, 180));
        }

        let mut confirmed_facts = Vec::new();
        for msg in self.session.messages.iter().rev() {
            for block in &msg.content {
                if let ContentBlock::ToolResult { content, .. } = block {
                    if content.starts_with("Error:") {
                        continue;
                    }
                    confirmed_facts.push(summarize_text(content, 180));
                    if confirmed_facts.len() >= 4 {
                        break;
                    }
                }
            }
            if confirmed_facts.len() >= 4 {
                break;
            }
        }

        let open_loops: Vec<String> = turn
            .tool_calls
            .iter()
            .rev()
            .filter_map(|call| {
                call.error
                    .as_ref()
                    .map(|error| format!("{}: {}", call.name, summarize_text(error, 180)))
            })
            .take(4)
            .collect();

        let pending_actions: Vec<String> = if open_loops.is_empty() {
            vec!["Continue with next smallest verifiable step".to_string()]
        } else {
            vec![
                "Re-evaluate failed tool steps with narrower scope".to_string(),
                "Re-derive plan from canonical facts before further edits".to_string(),
            ]
        };

        let mut critical_refs = self
            .session
            .working_set
            .lock()
            .expect("working_set poisoned")
            .top_paths(8);
        for tool_call in turn.tool_calls.iter().rev().take(4) {
            critical_refs.push(format!("tool:{}", tool_call.id));
        }
        critical_refs.dedup();

        CanonicalState {
            goal,
            constraints,
            confirmed_facts,
            open_loops,
            pending_actions,
            critical_refs,
        }
    }

    pub fn canonical_prompt(
        &self,
        canonical: &CanonicalState,
        pointer: &str,
        action: GuardrailAction,
        extra: Option<&str>,
    ) -> SystemPrompt {
        let mut lines = vec![
            COMPACTION_SUMMARY_MARKER.to_string(),
            format!("Capacity Canonical State [{}]", action.as_str()),
            format!("Goal: {}", canonical.goal),
            "Constraints:".to_string(),
        ];
        for item in &canonical.constraints {
            lines.push(format!("- {}", summarize_text(item, 200)));
        }
        lines.push("Confirmed Facts:".to_string());
        for item in &canonical.confirmed_facts {
            lines.push(format!("- {}", summarize_text(item, 200)));
        }
        lines.push("Open Loops:".to_string());
        if canonical.open_loops.is_empty() {
            lines.push("- none".to_string());
        } else {
            for item in &canonical.open_loops {
                lines.push(format!("- {}", summarize_text(item, 200)));
            }
        }
        lines.push("Pending Actions:".to_string());
        for item in &canonical.pending_actions {
            lines.push(format!("- {}", summarize_text(item, 200)));
        }
        lines.push("Critical Refs:".to_string());
        for item in &canonical.critical_refs {
            lines.push(format!("- {}", summarize_text(item, 200)));
        }
        if let Some(extra) = extra {
            lines.push(format!("Instruction: {}", summarize_text(extra, 240)));
        }
        lines.push(format!("Memory Pointer: {pointer}"));

        SystemPrompt::Blocks(vec![crate::models::SystemBlock {
            block_type: "text".to_string(),
            text: lines.join("\n"),
            cache_control: None,
        }])
    }

    pub fn capacity_source_message_ids(&self, turn: &TurnContext) -> Vec<String> {
        let mut ids: Vec<String> = turn
            .tool_calls
            .iter()
            .rev()
            .take(8)
            .map(|call| call.id.clone())
            .collect();
        ids.reverse();
        ids
    }

    pub fn build_capacity_record(
        &self,
        turn: &TurnContext,
        action: GuardrailAction,
        snapshot: Option<&CapacitySnapshot>,
        canonical: CanonicalState,
        source_message_ids: Vec<String>,
        replay_info: Option<ReplayInfo>,
    ) -> CapacityMemoryRecord {
        let (h_hat, c_hat, slack, risk_band) = snapshot
            .map(|s| (s.h_hat, s.c_hat, s.slack, s.risk_band.as_str().to_string()))
            .unwrap_or_else(|| (0.0, 0.0, 0.0, "unknown".to_string()));

        CapacityMemoryRecord {
            id: new_record_id(),
            ts: now_rfc3339(),
            turn_index: self.turn_counter,
            action_trigger: action.as_str().to_string(),
            h_hat,
            c_hat,
            slack,
            risk_band,
            canonical_state: canonical,
            source_message_ids: if source_message_ids.is_empty() {
                vec![turn.id.clone()]
            } else {
                source_message_ids
            },
            replay_info,
        }
    }

    pub async fn persist_capacity_record(
        &mut self,
        turn: &TurnContext,
        action: GuardrailAction,
        record: &CapacityMemoryRecord,
    ) -> String {
        let pointer = format!("memory://{}/{}", self.session.id, record.id);
        if let Err(err) = append_capacity_record(&self.session.id, record) {
            let event = Event::CapacityMemoryPersistFailed {
                session_id: VerifiedAnalyticsMetadata::verified(
                    &self.session.telemetry_session_id,
                ),
                turn_id: VerifiedAnalyticsMetadata::verified(&turn.id),
                action: VerifiedAnalyticsMetadata::verified(action.as_str()),
                error: RedactedAnalyticsMetadata::redact(&summarize_text(&err.to_string(), 280)),
            };
            self.emit_telemetry(&event);
            let _ = self.tx_event.send(event).await;
            return format!("{pointer}?persist=failed");
        }
        pointer
    }

    pub fn rehydrate_latest_canonical_state(&mut self) {
        let Ok(records) = load_last_k_capacity_records(&self.session.id, 1) else {
            return;
        };
        let Some(last) = records.last() else {
            return;
        };
        let pointer = format!("memory://{}/{}", self.session.id, last.id);
        let prompt = self.canonical_prompt(
            &last.canonical_state,
            &pointer,
            GuardrailAction::NoIntervention,
            Some("Rehydrated canonical state from memory."),
        );
        self.merge_compaction_summary(Some(prompt));
    }
}
