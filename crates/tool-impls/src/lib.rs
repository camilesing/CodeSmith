//! Model-visible tool implementations for CodeSmith.
//!
//! Concrete `impl ToolSpec` blocks live in this crate, migrated from the
//! TUI's `tools/` subtree. The crate depends on
//! [`codesmith_agent_runtime`] for the `ToolSpec` / `ToolContext` /
//! `ToolResult` contracts and the trait-erased runtime services (shell,
//! task, automation, hook, notifier, background-task registry, …); it never
//! depends on the TUI, so the engine closure stays portable across hosts
//! (TUI today, app-server tomorrow).
//!
//! # Safety posture
//!
//! `#![deny(unsafe_code)]` mirrors the [`codesmith_agent_runtime`] safety
//! posture. Any tool that genuinely needs `unsafe` opts in at the file level
//! with a `SAFETY` note — see `codesmith_agent_runtime::lib.rs` (module doc,
//! "Safety") for the convention.

#![deny(unsafe_code)]

pub mod tools;
