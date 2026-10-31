//! Session context plane — the DSH `context/*` counterpart, split out of
//! `codesmith-agent-runtime` in round 9.
//!
//! - [`project_docs`] / [`claudemd`]: AGENTS/CLAUDE.md discovery up the
//!   parent chain and the memory-excludes policy.
//! - [`working_set`] / [`workspace_discovery`]: the recently-touched path
//!   set rendered into `<turn_meta>`.
//! - [`workspace_trust`] / [`settings_file`] / [`onboarding`]: the
//!   trust/onboarding decision and its persisted state.
//! - [`memory`] / [`knowledge`] / [`agent_memory`]: user memory,
//!   knowledge-on-demand, and per-agent memory directories.
//!
//! The module historically named `project_context` is [`project_docs`]
//! here; the engine re-exports it under the old name.

pub mod agent_memory;
pub mod claudemd;
pub mod knowledge;
pub mod memory;
pub mod onboarding;
pub mod project_docs;
pub mod settings_file;
pub mod working_set;
pub mod workspace_discovery;
pub mod workspace_trust;

#[cfg(test)]
mod test_support;
