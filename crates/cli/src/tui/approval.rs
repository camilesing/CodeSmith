//! Compatibility shim: the approval policy enum lives in
//! `codesmith-agent-runtime::mode`; the modal views were removed with the
//! ratatui frontend.

pub use codesmith_agent_runtime::mode::ApprovalMode;
