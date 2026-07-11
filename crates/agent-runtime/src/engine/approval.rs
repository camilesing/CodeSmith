//! Approval + user-input handshake for the agent loop.
//!
//! Extracted from `core/engine.rs` (P1.3). The agent loop blocks on these
//! two futures whenever a tool requires explicit approval (`await_tool_approval`)
//! or whenever a tool requests live user input (`await_user_input`). Channels
//! and engine state stay private to the parent module.

use crate::user_input::UserInputResponse;

#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    Approved {
        id: String,
    },
    Denied {
        id: String,
    },
    /// Retry a tool with an elevated sandbox policy.
    RetryWithPolicy {
        id: String,
        policy: crate::sandbox::SandboxPolicy,
    },
}

#[derive(Debug, Clone)]
pub enum UserInputDecision {
    Submitted {
        id: String,
        response: UserInputResponse,
    },
    Cancelled {
        id: String,
    },
}
