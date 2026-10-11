//! Re-export of [`codesmith_agent::llm_client`] (extracted in Phase 6 §6a).
//!
//! The canonical home for the `LlmClient` trait, `RetryConfig`, and the
//! `with_retry` helper is now the `codesmith_agent` crate. The test-only
//! [`mock`] module stays in the TUI — dependencies are not built with
//! `cfg(test)`, so `MockLlmClient` cannot live in a downstream crate. It
//! implements the re-exported `LlmClient` trait via this shim.

pub use codesmith_agent::llm_client::*;

#[cfg(test)]
pub mod mock;
