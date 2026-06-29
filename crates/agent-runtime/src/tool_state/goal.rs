//! State types extracted from `crates/tui/src/tools/goal.rs`.
//! Tool implementations stay in tui.

use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Maximum number of automatic goal-continuation prompt injections in one
/// engine turn. This prevents a missing `update_goal` call from becoming an
/// unbounded local loop.
pub const MAX_GOAL_CONTINUATIONS_PER_TURN: u32 = 3;

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
}

impl GoalStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Complete => "complete",
            Self::Blocked => "blocked",
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
                if changed {
                    self.objective = Some(objective.to_string());
                    self.token_budget = token_budget;
                    self.started_at = Some(Instant::now());
                    self.evidence = None;
                    self.blocker = None;
                } else if token_budget.is_some() {
                    self.token_budget = token_budget;
                }

                if changed || self.status.is_none() {
                    self.status = Some(if completed {
                        GoalStatus::Complete
                    } else {
                        GoalStatus::Active
                    });
                    self.finished_at = completed.then(Instant::now);
                }
            }
            None => self.clear(),
        }
    }

    pub fn create(&mut self, objective: String, token_budget: Option<u32>) {
        self.objective = Some(objective);
        self.token_budget = token_budget;
        self.status = Some(GoalStatus::Active);
        self.started_at = Some(Instant::now());
        self.finished_at = None;
        self.evidence = None;
        self.blocker = None;
    }

    pub fn resume(&mut self, objective: Option<String>) -> Result<(), &'static str> {
        if let Some(objective) = objective {
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
        if self.objective.is_none() {
            return Err("No active goal exists to complete.");
        }
        self.status = Some(GoalStatus::Complete);
        self.finished_at = Some(Instant::now());
        self.evidence = Some(evidence);
        self.blocker = None;
        Ok(())
    }

    pub fn mark_blocked(&mut self, blocker: String) -> Result<(), &'static str> {
        if self.objective.is_none() {
            return Err("No active goal exists to block.");
        }
        self.status = Some(GoalStatus::Blocked);
        self.finished_at = Some(Instant::now());
        self.blocker = Some(blocker);
        Ok(())
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
            elapsed_seconds: self.started_at.map(|started| started.elapsed().as_secs()),
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
