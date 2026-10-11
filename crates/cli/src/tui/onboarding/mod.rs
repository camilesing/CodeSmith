//! Compatibility shim: the onboarding marker and trust-gate inputs live in
//! `codesmith_agent_runtime::onboarding`; the first-run wizard runs as CLI
//! questions in the `codesmith` dispatcher and the re-entry gates were
//! removed with the ratatui frontend.

pub use codesmith_agent_runtime::onboarding::{is_onboarded, mark_onboarded, needs_trust};
