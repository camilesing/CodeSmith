//! Long-lived Python REPL runtime used by the RLM loop and by inline
//! `` ```repl `` block execution in the agent loop.
//!
//! Migrated to `codesmith_agent_runtime::repl`; re-exported here only as an
//! external-API alias (`lib.rs` exposes `pub mod repl`). No in-crate callers
//! remain — the consumers (`rlm/bridge.rs`, `rlm/session.rs`, `rlm/turn.rs`)
//! live in `codesmith-agent-runtime` and use that crate's `repl` module
//! directly — so nothing in `codesmith-cli` depends on this shim.
//!
//! The compatibility surface mirrors the explicit list curated by
//! `codesmith_agent_runtime::repl` itself.

pub use codesmith_agent_runtime::repl::runtime;
pub use codesmith_agent_runtime::repl::sandbox;
pub use codesmith_agent_runtime::repl::sandbox::{ReplBlock, extract_repl_blocks, has_repl_block};
pub use codesmith_agent_runtime::repl::{
    BatchResp, PythonRuntime, ReplRound, RpcDispatcher, RpcRequest, RpcResponse, SingleResp,
};
