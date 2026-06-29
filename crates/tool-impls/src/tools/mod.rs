//! Concrete tool implementations (`impl ToolSpec`).
//!
//! Each submodule is one tool (or a small family) migrated from the TUI's
//! `tools/` subtree. The `ToolSpec` trait, `ToolContext`, `ToolResult`, and
//! the trait-erased `RuntimeToolServices` live upstream in
//! [`codesmith_agent_runtime::tools::spec`]; the modules here only provide
//! the concrete `impl ToolSpec` blocks and their helpers.
//!
//! Migration conventions (per file):
//! - `use crate::<migrated-agent-runtime-module>` →
//!   `use codesmith_agent_runtime::<...>`.
//! - Sibling `crate::tools::<sibling>` references stay `crate::tools::<...>`
//!   (the sibling has moved into this crate alongside the tool).
//! - `use super::spec::{...}` → `use codesmith_agent_runtime::tools::spec::{...}`.
//! - External crates (`codesmith_config`, `reqwest`, `serde_json`, …) keep
//!   their paths unchanged.

pub mod notify;
pub mod apply_patch;
pub mod parallel;
pub mod validate_data;
