//! State types extracted from `crates/tui/src/tools/plan_mode.rs`.
//! Tool implementations stay in `codesmith-tool-impls`.

use std::sync::Arc;
use tokio::sync::Mutex;

/// Session-level plan mode state, stored in Engine.
///
/// Holds only what the turn's `AppMode` cannot express: whether the *model*
/// opened a plan session of its own accord, and which plan file that session
/// writes to.
///
/// User-initiated plan mode is `AppMode::Plan` on the turn itself. That axis
/// belongs to whoever submits the turn (Shift+Tab, `/mode plan`, a preset, a
/// headless run) and is deliberately not mirrored here — a copy would be a
/// second source of truth that only stays right while every writer remembers
/// to update it. Tools serving both axes read `ToolContext::mode` and OR it
/// with this state.
#[derive(Debug, Clone, Default)]
pub struct PlanModeState {
    /// Whether a model-initiated plan session is open.
    ///
    /// Set by `enter_plan_mode` and cleared by `exit_plan_mode`, and neither
    /// happens while the turn is already in `AppMode::Plan`, where plan mode
    /// is the session's own state and needs no separate flag. No user mode
    /// switch writes it.
    pub active: bool,
    /// Slug for the current plan file (e.g. "plan_a3f2b1c4").
    pub current_slug: Option<String>,
}

/// Shared reference to plan mode state.
pub type SharedPlanModeState = Arc<Mutex<PlanModeState>>;

/// Create a new shared `PlanModeState`.
pub fn new_shared_plan_mode_state() -> SharedPlanModeState {
    Arc::new(Mutex::new(PlanModeState::default()))
}
