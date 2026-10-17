//! Re-export of [`codesmith_agent::llm_client`] (extracted in Phase 6 §6a).
//!
//! The canonical home for the `LlmClient` trait, `RetryConfig`, and the
//! `with_retry` helper is now the `codesmith_agent` crate. The test-only
//! [`mock`] module re-exports the shared `MockLlmClient` from
//! `codesmith_agent_runtime::test_support` (test-support feature on the
//! dev-dependency).

pub use codesmith_agent::llm_client::*;

#[cfg(test)]
pub use codesmith_agent_runtime::test_support::mock_llm as mock;
