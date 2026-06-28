//! State types extracted from `crates/tui/src/tools/plan_mode.rs`.
//! Tool implementations stay in tui.

use std::sync::Arc;
use tokio::sync::Mutex;

/// Session-level plan mode state, stored in Engine.
#[derive(Debug, Clone, Default)]
pub struct PlanModeState {
    /// Whether plan mode is currently active (model-initiated).
    pub active: bool,
    /// The AppMode variant name that was active before entering plan mode.
    /// Saved as a string so the state is Clone-friendly without requiring
    /// AppMode to be Clone (it may carry non-Clone fields in the future).
    pub pre_plan_mode: Option<String>,
    /// Slug for the current plan file (e.g. "plan_a3f2b1c4").
    pub current_slug: Option<String>,
    /// Whether this plan mode entry was model-initiated (tool call)
    /// vs user-initiated (/mode plan UI switch).
    pub model_initiated: bool,
}

/// Shared reference to plan mode state.
pub type SharedPlanModeState = Arc<Mutex<PlanModeState>>;

/// Create a new shared `PlanModeState`.
pub fn new_shared_plan_mode_state() -> SharedPlanModeState {
    Arc::new(Mutex::new(PlanModeState::default()))
}

