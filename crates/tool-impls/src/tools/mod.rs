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
pub mod finance;
pub mod fim;
pub mod project;
pub mod diagnostics;
pub mod knowledge_recall;
pub mod recall_archive;
pub mod review;
pub mod revert_turn;
pub mod web_run;
pub mod fetch_url;
pub mod git;
pub mod git_history;
pub mod handle;
pub mod image_ocr;
pub mod pandoc;
pub mod plan;
pub mod plan_file;
pub mod search;
pub mod test_runner;
pub mod todo;
pub mod tool_result_retrieval;
