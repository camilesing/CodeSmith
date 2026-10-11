//! Re-export of [`codesmith_agent_runtime::skills`] (Phase C6-1).
//!
//! Canonical home is now `codesmith_agent_runtime`. This glob shim
//! flattens the runtime module's public items — including the `install`
//! submodule — so `crate::skills::<item>` and `crate::skills::install::<item>`
//! paths in the TUI keep working until later steps rewire them onto the
//! runtime crate directly.

pub use codesmith_agent_runtime::skills::install;
pub use codesmith_agent_runtime::skills::*;
