//! Events emitted by the core engine to the UI.
//!
//! Re-exported from `codesmith_agent_runtime::events` so the engine and TUI
//! share the canonical `Event` / `TurnOutcomeStatus` types. The
//! implementation (including the `Event::error` / `Event::status`
//! constructors) lives in the runtime; the TUI only consumes it.
// TODO(shim-removal): remove this shim once the remaining call sites are
// rewired onto `codesmith_agent_runtime::events` directly.
pub use codesmith_agent_runtime::events::{Event, TurnOutcomeStatus};
