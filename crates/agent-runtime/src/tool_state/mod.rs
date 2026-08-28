//! Shared state types for model-visible tools.
//!
//! State types (plain data structs + Shared* aliases) are extracted here
//! so `EngineConfig` can reference them without a tui dependency. Tool
//! implementations (`impl ToolSpec`) stay in tui.

pub mod goal;
pub mod plan;
pub mod plan_mode;
pub mod task_v2;
pub mod team;
pub mod todo;
pub mod worktree;
