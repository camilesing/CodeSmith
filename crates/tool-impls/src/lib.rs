//! Model-visible tool implementations for CodeSmith.
//!
//! Tool implementations run inside the host's runtime (the interactive REPL
//! today): raw `print!` / `eprintln!` corrupt the host's rendering (the
//! #1085 "scroll demon"). Route output through `tracing_*` or the tool
//! result instead. Note these clippy lints fire under `cargo clippy`,
//! not plain `cargo build`.
#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]
// Test code may print skip notices (e.g. "pandoc not installed"); that
// never executes inside the host.
#![cfg_attr(test, allow(clippy::print_stdout, clippy::print_stderr))]
//!
//! Concrete `impl ToolSpec` blocks live in this crate, migrated from the
//! TUI's `tools/` subtree. The crate depends on
//! [`codesmith_agent_runtime`] for the `ToolSpec` / `ToolContext` /
//! `ToolResult` contracts and the trait-erased runtime services (shell,
//! task, automation, hook, notifier, background-task registry, …); it never
//! depends on the host crate, so the engine closure stays portable across hosts
//! (CLI today, app-server tomorrow).
//!
//! # Safety posture
//!
//! `#![deny(unsafe_code)]` mirrors the [`codesmith_agent_runtime`] safety
//! posture. Any tool that genuinely needs `unsafe` opts in at the file level
//! with a `SAFETY` note — see `codesmith_agent_runtime::lib.rs` (module doc,
//! "Safety") for the convention.

#![deny(unsafe_code)]

pub mod tools;
pub mod web_service;
