//! MCP client pool — the DSH `mcp/*` counterpart, split out of
//! `codesmith-agent-runtime` in round 9. (`codesmith-mcp`, the pre-existing
//! `crates/mcp`, is the *server* side — stdio server + config types — and is
//! unchanged.)
//!
//! - [`pool`]: the connection pool over stdio / Streamable-HTTP / SSE /
//!   WebSocket transports, tool discovery, per-call timeouts, and the
//!   disconnect/reconnect semantics (see that module's docs).
//! - [`oauth`]: the OAuth device flow and token cache for HTTP servers.
//!
//! The engine re-exports the pool as `agent_runtime::mcp` and the OAuth
//! module as `agent_runtime::mcp_oauth`.

pub mod oauth;
pub mod pool;

#[cfg(test)]
mod test_support;

pub use oauth::{McpOAuthConfig, McpOAuthToken, OAuthState};
pub use pool::{McpPool, McpServerConfig, McpTool, McpTransportKind};
