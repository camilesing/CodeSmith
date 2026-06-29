//! Tool infrastructure: specs, registry, and shared utilities.
//!
//! This module hosts the terminal-agnostic tool infrastructure that both the
//! engine body (in `codesmith-agent-runtime`) and concrete tool implementations
//! (in `codesmith-tool-impls`) depend on:
//!
//! - `spec` — `ToolSpec` trait, `ToolContext`, `RuntimeToolServices`
//! - `registry` — `ToolRegistry`, `ToolRegistryBuilder`
//! - Utility modules (`arg_repair`, `schema_sanitize`, …)
//!
//! Concrete `impl ToolSpec` blocks live in the downstream `codesmith-tool-impls`
//! crate so agent-runtime never depends on them (avoiding a circular dependency).

pub mod approval_cache;
pub mod arg_repair;
pub mod automation_types;
pub mod cargo_failure_summary;
pub mod diff_format;
pub mod git_env;
pub mod handle;
pub mod js_execution;
pub mod large_output_router;
pub mod registry;
pub mod schema_sanitize;
pub mod shell_output;
pub mod shell_types;
pub mod spec;
pub mod task_types;
pub mod truncate;
