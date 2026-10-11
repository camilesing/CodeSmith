//! Tool-output spillover writer (re-export shim).
//!
//! Canonical home is now `codesmith_agent_runtime::tools::truncate`; this glob
//! shim flattens the runtime module's public items so historical
//! `crate::tools::truncate::<item>` paths keep resolving.
pub use codesmith_agent_runtime::tools::truncate::*;
