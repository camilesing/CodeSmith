//! Shared state types for model-visible tools.
//!
//! State types (plain data structs + Shared* aliases) are extracted here
//! so `EngineConfig` can reference them without a tui dependency. Tool
//! implementations (`impl ToolSpec`) stay in tui.

pub mod plan;
pub mod todo;
