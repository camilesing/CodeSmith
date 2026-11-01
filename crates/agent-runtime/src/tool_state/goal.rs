//! State types extracted from `crates/tui/src/tools/goal.rs`.
//! Tool implementations stay in the CLI.

use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Maximum number of automatic goal-continuation prompt injections in one
/// engine turn. This prevents a missing `update_goal` call from becoming an
/// unbounded local loop.
pub const MAX_GOAL_CONTINUATIONS_PER_TURN: u32 = 3;

/// Hard cap on goal rounds the round driver may synthesize across turns
/// (round 10 TODO-71), for the same reason as the per-turn cap: a goal whose
/// model never calls `update_goal` must not burn tokens forever. The token
/// budget (`token_budget`) is the finer-grained bound when set; this cap is
/// the unconditional backstop.
pub const MAX_GOAL_ROUNDS: u32 = 25;

/// What the round driver does after one clean turn end (round 10 TODO-71).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundDecision {
    /// Synthesize round `u32` (1-based); the continuation prompt carries it.
    Drive(u32),
    /// Pause the goal; the string is the human reason.
    Pause(&'static str),
}

/// Shared reference to the current runtime goal.
pub type SharedGoalState = Arc<Mutex<GoalState>>;

/// Create an empty shared goal state.
#[must_use]
pub fn new_shared_goal_state() -> SharedGoalState {
    Arc::new(Mutex::new(GoalState::default()))
}

/// Create shared state seeded from the existing `/goal` surface.
#[must_use]
pub fn new_shared_goal_state_from_host(
    objective: Option<String>,
    token_budget: Option<u32>,
    completed: bool,
) -> SharedGoalState {
    let mut state = GoalState::default();
    state.sync_from_host(objective.as_deref(), token_budget, completed);
    Arc::new(Mutex::new(state))
}

/// Runtime status for a goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalStatus {
    Active,
    Complete,
    Blocked,
    /// Stopped by the round driver, not by the model (round 10 TODO-71):
    /// token budget exhausted or the round cap reached. `resume` (or a new
    /// budget via `/goal` / `update_goal`) makes it active again.
    Paused,
}

impl GoalStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Complete => "complete",
            Self::Blocked => "blocked",
            Self::Paused => "paused",
        }
    }
}

/// Session-local goal state. `Instant` stays runtime-only; snapshots expose
/// elapsed seconds so tool output remains serializable and stable.
#[derive(Debug, Clone, Default)]
pub struct GoalState {
    objective: Option<String>,
    token_budget: Option<u32>,
    status: Option<GoalStatus>,
    started_at: Option<Instant>,
    finished_at: Option<Instant>,
    evidence: Option<String>,
    blocker: Option<String>,
    /// Rounds the round driver has synthesized so far (round 10 TODO-71).
    /// Reset by `create` and by a changed objective in `sync_from_host`.
    rounds_driven: u32,
    /// Turn usage accumulated while the goal was active (input + output).
    /// The driver pauses the goal when this reaches `token_budget`.
    tokens_spent: u64,
}

impl GoalState {
    #[must_use]
    pub fn objective(&self) -> Option<&str> {
        self.objective.as_deref()
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.objective.is_some() && self.status == Some(GoalStatus::Active)
    }

    pub fn sync_from_host(
        &mut self,
        objective: Option<&str>,
        token_budget: Option<u32>,
        completed: bool,
    ) {
        let objective = objective.map(str::trim).filter(|value| !value.is_empty());
        match objective {
            Some(objective) => {
                let changed = self.objective.as_deref() != Some(objective);
                // token_budget asymmetry: a changed objective is a fresh
                // goal, so the host's budget (even `None`) replaces any
                // locally-set one; an unchanged objective only adopts a
                // `Some` budget so a host that doesn't track budgets can't
                // clear what was set locally.
                if changed {
                    self.objective = Some(objective.to_string());
                    self.token_budget = token_budget;
                    self.started_at = Some(Instant::now());
                    self.evidence = None;
                    self.blocker = None;
                    self.rounds_driven = 0;
                    self.tokens_spent = 0;
                } else if token_budget.is_some() {
                    self.token_budget = token_budget;
                }

                // Diff the status, not just its presence: ignoring
                // `completed` when the objective is unchanged and a status
                // exists let host and runtime silently diverge, so
                // continuation prompts kept firing for a goal the host
                // already reported done.
                let new_status = if completed {
                    GoalStatus::Complete
                } else {
                    GoalStatus::Active
                };
                // The host's objective/budget/completed surface cannot
                // represent `Blocked` (set at runtime via `mark_blocked`):
                // for an unchanged objective, a "not completed" host sync
                // must not silently un-block the goal and restart
                // continuation prompts. Host-driven un-blocking is a
                // changed objective (handled above) or `completed`.
                let keep_blocked =
                    !completed && !changed && self.status == Some(GoalStatus::Blocked);
                if !keep_blocked
                    && (changed || self.status.is_none() || self.status != Some(new_status))
                {
                    self.status = Some(new_status);
                    self.finished_at = completed.then(Instant::now);
                    // Reconcile terminal fields on every host-driven
                    // transition: leaving `Blocked` drops a stale blocker
                    // string, leaving `Complete` drops stale evidence.
                    if new_status != GoalStatus::Blocked {
                        self.blocker = None;
                    }
                    if new_status != GoalStatus::Complete {
                        self.evidence = None;
                    }
                }
            }
            None => self.clear(),
        }
    }

    pub fn create(&mut self, objective: String, token_budget: Option<u32>) {
        let objective = objective.trim();
        if objective.is_empty() {
            return;
        }
        self.objective = Some(objective.to_string());
        self.token_budget = token_budget;
        self.status = Some(GoalStatus::Active);
        self.started_at = Some(Instant::now());
        self.finished_at = None;
        self.evidence = None;
        self.blocker = None;
        self.rounds_driven = 0;
        self.tokens_spent = 0;
    }

    pub fn resume(&mut self, objective: Option<String>) -> Result<(), &'static str> {
        if let Some(objective) = objective {
            if self.objective.as_deref() == Some(objective.trim()) {
                self.status = Some(GoalStatus::Active);
                self.finished_at = None;
                self.evidence = None;
                self.blocker = None;
                return Ok(());
            }
            self.create(objective, self.token_budget);
            return Ok(());
        }
        if self.objective.is_none() {
            return Err("No goal exists to resume.");
        }
        self.status = Some(GoalStatus::Active);
        self.finished_at = None;
        self.evidence = None;
        self.blocker = None;
        Ok(())
    }

    pub fn mark_complete(&mut self, evidence: String) -> Result<(), &'static str> {
        if !self.is_active() {
            return Err("No active goal exists to complete.");
        }
        self.status = Some(GoalStatus::Complete);
        self.finished_at = Some(Instant::now());
        self.evidence = Some(evidence);
        self.blocker = None;
        Ok(())
    }

    pub fn mark_blocked(&mut self, blocker: String) -> Result<(), &'static str> {
        if !self.is_active() {
            return Err("No active goal exists to block.");
        }
        self.status = Some(GoalStatus::Blocked);
        self.finished_at = Some(Instant::now());
        self.evidence = None;
        self.blocker = Some(blocker);
        Ok(())
    }

    /// Pause the goal, recording why the round driver stopped it (round 10
    /// TODO-71). The reason rides the `blocker` field (snapshot-visible), so
    /// `/goal` and `get_goal` surface it without a new field.
    pub fn pause(&mut self, reason: &str) {
        self.status = Some(GoalStatus::Paused);
        self.finished_at = Some(Instant::now());
        self.blocker = Some(reason.to_string());
    }

    /// Round-driver decision after a clean turn end while the goal is active
    /// (round 10 TODO-71). Accumulates the finished turn's usage into the
    /// goal's spend (every turn while the goal is active counts — the budget
    /// bounds the goal's total pursuit, autonomous or not), then either
    /// grants the next round's index or names why the goal pauses.
    pub fn next_round(&mut self, usage_input: u64, usage_output: u64) -> RoundDecision {
        self.tokens_spent = self.tokens_spent.saturating_add(usage_input + usage_output);
        if let Some(budget) = self.token_budget
            && self.tokens_spent >= u64::from(budget)
        {
            return RoundDecision::Pause("token budget exhausted");
        }
        if self.rounds_driven >= MAX_GOAL_ROUNDS {
            return RoundDecision::Pause("round limit reached");
        }
        self.rounds_driven += 1;
        RoundDecision::Drive(self.rounds_driven)
    }

    /// Rounds the driver has synthesized so far (snapshot/test surface).
    #[must_use]
    pub fn rounds_driven(&self) -> u32 {
        self.rounds_driven
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    #[must_use]
    pub fn snapshot(&self) -> GoalSnapshot {
        GoalSnapshot {
            objective: self.objective.clone(),
            status: self
                .status
                .map(GoalStatus::as_str)
                .unwrap_or("none")
                .to_string(),
            token_budget: self.token_budget,
            elapsed_seconds: self.started_at.map(|started| {
                let end = self.finished_at.unwrap_or_else(Instant::now);
                end.duration_since(started).as_secs()
            }),
            evidence: self.evidence.clone(),
            blocker: self.blocker.clone(),
        }
    }
}

/// Serializable tool output and prompt input for the current goal.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GoalSnapshot {
    pub objective: Option<String>,
    pub status: String,
    pub token_budget: Option<u32>,
    pub elapsed_seconds: Option<u64>,
    pub evidence: Option<String>,
    pub blocker: Option<String>,
}

impl GoalSnapshot {
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.objective.is_some() && self.status == GoalStatus::Active.as_str()
    }
}

/// Render the bounded continuation prompt injected when a goal is still active
/// after an assistant message has no tool calls.
#[must_use]
pub fn render_continuation_prompt(
    snapshot: &GoalSnapshot,
    continuation_index: u32,
    max_continuations: u32,
) -> String {
    let goal_json = serde_json::to_string_pretty(snapshot)
        .unwrap_or_else(|_| {
            // Unreachable with the current all-infallibly-serializable
            // fields, but the fallback replacing the whole state with `{}`
            // must at least be observable if it ever fires.
            tracing::warn!(
                target: "codesmith::goal",
                "goal snapshot serialization failed; continuation prompt gets empty state"
            );
            "{}".to_string()
        })
        // `objective`/`evidence`/`blocker` are model-controlled strings
        // landing inside a ```json fence; a backtick run in them could
        // close the fence for consumers that don't parse Markdown
        // strictly, restructuring the instruction block that drives
        // continuation decisions. This is prompt text, not a stored
        // record — defusing backticks loses nothing durable.
        .replace('`', "'");
    format!(
        "{}\n\n## Active Goal State\n\n```json\n{}\n```\n\nContinuation pass: {}/{}.\nIf the goal is complete, call `update_goal` with `status: \"complete\"` and concrete evidence. If it is blocked, call `update_goal` with `status: \"blocked\"` and the blocker. Otherwise continue making progress toward the objective.",
        crate::prompts::GOAL_CONTINUATION_PROMPT.trim(),
        goal_json,
        continuation_index,
        max_continuations,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round 10 TODO-71 — the round driver's decision sequence.
    #[test]
    fn next_round_drives_then_pauses_on_budget() {
        let mut state = GoalState::default();
        state.create("ship it".to_string(), Some(100));
        // Rounds accumulate spend; 3 rounds × 40 = 120 ≥ 100 → pause.
        assert_eq!(state.next_round(20, 20), RoundDecision::Drive(1));
        assert_eq!(state.next_round(20, 20), RoundDecision::Drive(2));
        // The engine applies the pause (goal_round_after_turn calls pause).
        let decision = state.next_round(20, 20);
        assert_eq!(decision, RoundDecision::Pause("token budget exhausted"));
        if let RoundDecision::Pause(reason) = decision {
            state.pause(reason);
        }
        assert_eq!(state.status, Some(GoalStatus::Paused));
        assert_eq!(state.blocker.as_deref(), Some("token budget exhausted"));
        // Paused is not active: the driver stops calling, and the snapshot
        // says so.
        assert!(!state.snapshot().is_active());
        assert_eq!(state.snapshot().status, "paused");
        // Resume makes it active again (and a raised budget lets it drive).
        state.resume(None).expect("resume");
        assert!(state.is_active());
        assert_eq!(
            state.next_round(0, 0),
            RoundDecision::Pause("token budget exhausted")
        );
    }

    #[test]
    fn next_round_hits_round_cap_without_budget() {
        let mut state = GoalState::default();
        state.create("endless".to_string(), None);
        for expected in 1..=MAX_GOAL_ROUNDS {
            assert_eq!(state.next_round(0, 0), RoundDecision::Drive(expected));
        }
        assert_eq!(
            state.next_round(0, 0),
            RoundDecision::Pause("round limit reached")
        );
    }

    #[test]
    fn create_resets_round_counters() {
        let mut state = GoalState::default();
        state.create("first".to_string(), Some(50));
        assert_eq!(
            state.next_round(60, 0),
            RoundDecision::Pause("token budget exhausted")
        );
        state.create("second".to_string(), Some(10_000));
        assert_eq!(state.rounds_driven(), 0);
        assert_eq!(state.next_round(0, 0), RoundDecision::Drive(1));
    }

    #[test]
    fn sync_from_host_tracks_completion_on_unchanged_objective() {
        let mut state = GoalState::default();
        state.sync_from_host(Some("ship the release"), Some(1000), false);
        assert!(state.snapshot().is_active());

        // Unchanged objective + completed=true must flip the status: the old
        // guard ignored `completed` whenever a status already existed, so
        // host and runtime silently diverged and continuation prompts kept
        // firing for a goal the host reported done.
        state.sync_from_host(Some("ship the release"), None, true);
        assert!(!state.snapshot().is_active());
        assert_eq!(state.snapshot().status, "complete");

        // Syncing completed=true a second time on the same unchanged
        // objective stays complete — the goal is not resurrected.
        state.sync_from_host(Some("ship the release"), None, true);
        assert!(!state.snapshot().is_active());
        assert_eq!(state.snapshot().status, "complete");
    }

    #[test]
    fn sync_from_host_does_not_unblock_a_runtime_blocked_goal() {
        // `mark_blocked` is a runtime decision; the host's
        // objective/budget/completed surface cannot express "blocked", so
        // an unchanged "not completed" sync must not resurrect the goal
        // as active (that restarted continuation prompts every turn).
        let mut state = GoalState::default();
        state.sync_from_host(Some("ship the release"), Some(1000), false);
        state
            .mark_blocked("missing signing key".to_string())
            .unwrap();
        assert_eq!(state.snapshot().status, "blocked");

        state.sync_from_host(Some("ship the release"), None, false);
        assert_eq!(
            state.snapshot().status,
            "blocked",
            "host sync must not silently un-block a runtime-blocked goal"
        );

        // Completion still wins over Blocked, and the blocker string is
        // reconciled away with the transition.
        state.sync_from_host(Some("ship the release"), None, true);
        assert_eq!(state.snapshot().status, "complete");
        assert!(
            state.snapshot().blocker.is_none(),
            "leaving Blocked must drop the stale blocker"
        );
    }

    #[test]
    fn sync_from_host_transition_to_active_drops_stale_evidence() {
        let mut state = GoalState::default();
        state.sync_from_host(Some("ship the release"), Some(1000), false);
        state.sync_from_host(Some("ship the release"), None, true);
        assert_eq!(state.snapshot().status, "complete");

        // A changed objective restarts the goal as active; stale evidence
        // from the previous completion must not ride along.
        state.sync_from_host(Some("ship the next release"), None, false);
        assert_eq!(state.snapshot().status, "active");
        assert!(
            state.snapshot().evidence.is_none(),
            "leaving Complete must drop stale evidence"
        );
    }
}
