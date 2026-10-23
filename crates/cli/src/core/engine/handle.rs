//! Public `EngineHandle` methods.
//!
//! The struct itself lives next door in `engine.rs` because two
//! construction sites (`Engine::new` and the test-only
//! `mock_engine_handle`) need access to its private mpsc channels.
//! The method surface — `send`, `cancel*`, `is_cancelled`,
//! `approve_tool_call` / `deny_tool_call` / `retry_tool_with_policy`,
//! `submit_user_input` / `cancel_user_input`, and `steer` — moves here
//! so the agent loop's mailbox API is reviewable on its own.

use anyhow::Context as _;
use anyhow::Result;

use super::{ApprovalDecision, CancelReason, EngineHandle, Op, UserInputDecision};
use codesmith_agent_runtime::user_input::UserInputResponse;

impl EngineHandle {
    /// Send an operation to the engine
    pub async fn send(&self, op: Op) -> Result<()> {
        self.tx_op
            .send(op)
            .await
            .context("engine op mailbox closed; operation dropped")?;
        Ok(())
    }

    /// Cancel the current request (user-initiated path — keeps the
    /// public `cancel()` signature stable). Equivalent to
    /// `cancel_with_reason(CancelReason::User)`.
    pub fn cancel(&self) {
        self.cancel_with_reason(CancelReason::User);
    }

    /// Cancel the current request and latch the reason so downstream
    /// "request cancelled" error messages can name a cause.
    pub fn cancel_with_reason(&self, reason: CancelReason) {
        match self.cancel_reason.lock() {
            Ok(mut slot) => *slot = Some(reason),
            Err(poisoned) => *poisoned.into_inner() = Some(reason),
        }
        match self.cancel_token.lock() {
            Ok(token) => token.cancel(),
            Err(poisoned) => poisoned.into_inner().cancel(),
        }
        codesmith_agent_runtime::retry_status::clear();
    }

    /// Check if a request is currently cancelled
    #[must_use]
    #[allow(dead_code)]
    pub fn is_cancelled(&self) -> bool {
        match self.cancel_token.lock() {
            Ok(token) => token.is_cancelled(),
            Err(poisoned) => poisoned.into_inner().is_cancelled(),
        }
    }

    /// Approve a pending tool call
    pub async fn approve_tool_call(&self, id: impl Into<String>) -> Result<()> {
        let id = id.into();
        self.tx_approval
            .send(ApprovalDecision::Approved { id: id.clone() })
            .await
            .with_context(|| {
                format!("engine approval mailbox closed; approval for tool call {id:?} dropped")
            })?;
        Ok(())
    }

    /// Deny a pending tool call
    pub async fn deny_tool_call(&self, id: impl Into<String>) -> Result<()> {
        let id = id.into();
        self.tx_approval
            .send(ApprovalDecision::Denied { id: id.clone() })
            .await
            .with_context(|| {
                format!("engine approval mailbox closed; denial for tool call {id:?} dropped")
            })?;
        Ok(())
    }

    /// Retry a tool call with an elevated sandbox policy.
    pub async fn retry_tool_with_policy(
        &self,
        id: impl Into<String>,
        policy: crate::sandbox::SandboxPolicy,
    ) -> Result<()> {
        let id = id.into();
        self.tx_approval
            .send(ApprovalDecision::RetryWithPolicy {
                id: id.clone(),
                policy,
            })
            .await
            .with_context(|| {
                format!(
                    "engine approval mailbox closed; sandbox-retry policy for tool call {id:?} dropped"
                )
            })?;
        Ok(())
    }

    /// Submit a response for request_user_input.
    pub async fn submit_user_input(
        &self,
        id: impl Into<String>,
        response: UserInputResponse,
    ) -> Result<()> {
        let id = id.into();
        self.tx_user_input
            .send(UserInputDecision::Submitted {
                id: id.clone(),
                response,
            })
            .await
            .with_context(|| {
                format!("user-input mailbox closed; response for prompt {id:?} dropped")
            })?;
        Ok(())
    }

    /// Cancel a request_user_input prompt.
    pub async fn cancel_user_input(&self, id: impl Into<String>) -> Result<()> {
        let id = id.into();
        self.tx_user_input
            .send(UserInputDecision::Cancelled { id: id.clone() })
            .await
            .with_context(|| {
                format!("user-input mailbox closed; cancellation for prompt {id:?} dropped")
            })?;
        Ok(())
    }

    /// Steer an in-flight turn with additional user input.
    pub async fn steer(&self, content: impl Into<String>) -> Result<()> {
        self.tx_steer
            .send(content.into())
            .await
            .context("steer mailbox closed; steering input dropped")?;
        Ok(())
    }
}
