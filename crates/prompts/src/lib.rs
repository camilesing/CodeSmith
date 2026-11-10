//! System-prompt assembly — the general agent's `core/system-prompt` counterpart, split
//! out of `codesmith-agent-runtime` in round 9.
//!
//! - [`assembly`]: `BASE_PROMPT` + mode deltas + personality overlays +
//!   approval policies + project instructions + skills catalogue — the
//!   module historically named `prompts`.
//! - [`runtime`] / [`sources`]: prompt-section registration with stability
//!   and cache annotations, and the section sources.
//! - [`inspect`]: `/inspect`-style prompt rendering for debugging.
//! - [`prefix_cache`]: the `PrefixStabilityManager` drift check over the
//!   three-zone contract's pinned prefix.
//! - [`skills`]: skill discovery, the model-facing catalogue render, and
//!   built-in system skills.
//!
//! The engine re-exports everything under its historical paths
//! (`agent_runtime::{prompts, prompt_runtime, prompt_sources, prompt_inspect,
//! prefix_cache, skills}`).

pub mod assembly;
pub mod inspect;
pub mod prefix_cache;
pub mod runtime;
pub mod skills;

pub mod sources;
#[cfg(test)]
mod test_support;

pub use assembly::BASE_PROMPT;
