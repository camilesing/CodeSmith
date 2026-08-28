//! User-level memory file — re-export shim.
//!
//! The portable implementation (`load`, `as_system_block`, `compose_block`,
//! `compose_kod_block`, `append_entry`, …) physically lives in
//! [`codesmith_agent_runtime::memory`]; this module re-exports it so
//! historical `crate::memory::*` paths keep resolving.

pub use codesmith_agent_runtime::memory::*;
