# RFC: MCP Modularization

**Issue:** #2190
**Status:** Superseded — crate split dropped (decision record); OAuth implemented in `agent-runtime`
**Date:** 2026-05-26 · decision recorded 2026-08

## Original proposal (superseded)

This RFC originally proposed splitting `crates/mcp` into three crates —
`mcp-protocol` (shared types), `mcp-client` (client + OAuth), `mcp-server`
(stdio server) — motivated by client/server separation, OAuth support, and
reuse of the MCP client outside the TUI.

## Why the split was dropped

A 2026-08 code review invalidated the RFC's premise:

- **The real MCP client is not in `crates/mcp`.** It lives in
  `crates/agent-runtime/src/mcp.rs` (6,000+ lines: connection pooling, tool
  discovery, stdio/Streamable-HTTP/SSE/WebSocket transports, timeouts,
  policy gates). `crates/mcp` is a 1,400-line lifecycle/tool-proxy
  compatibility layer used by `crates/cli`; the TUI re-exports the
  agent-runtime client (`crates/tui/src/mcp.rs`).
- **The "reuse outside the TUI" motivation is already met.** The TUI,
  `exec` mode, runtime API, and tests all share the agent-runtime client.
  Extracting it again would duplicate, not enable, reuse.
- **The "stdio only" out-of-scope note was stale.** Streamable HTTP, SSE,
  and WebSocket transports already exist.

Splitting would add three crates of boundary churn with no consumer that
needs the seam. The remaining valid motivation — OAuth — did not require a
split, so it landed next to the real client.

## What was implemented instead: OAuth in `agent-runtime`

`crates/agent-runtime/src/mcp_oauth.rs` (2026-08):

- **Device Code Flow** (RFC 8628): GitHub provider preset plus custom
  `device_code_url`/`token_url` providers; `authorization_pending` /
  `slow_down` / expiry handling.
- **Token storage** via `codesmith-secrets` (`Secrets::auto_detect()`:
  system keyring when available, permissioned-file fallback), keyed
  `mcp_oauth/<server>`. Tokens never touch `mcp.json`.
- **Connect-time injection**: `McpServerConfig.oauth` (optional table).
  `McpConnection::connect_with_policy` resolves the stored token and
  injects `Authorization: Bearer` into the URL transports' header map —
  never overwriting a user-configured `headers.Authorization`.
- **401 single-shot refresh** in the HTTP transports
  (`StreamableHttpTransport`, `SseTransport`): one refresh-token round
  trip, mirroring the existing Claude.ai manual-refresh seam; the refresh
  only fires when the current header matches the stored token (i.e. we
  injected it).
- **CLI entry point**: `codesmith mcp auth <server>` runs the interactive
  device flow out-of-band (printed URL + user code, polling, store).
- WebSocket transport: injection-only (no 401 refresh seam exists there).

User docs: `docs/MCP.md` → “OAuth”.

## Remaining out of scope

- PKCE / redirect-based OAuth flows.
- Client-credentials (service-to-service) grants.
- Automatic token refresh daemons; refresh happens lazily at connect and
  on 401.
- MCP server discovery; tool-result streaming; server-side tool approval
  flows (unchanged from the original RFC's out-of-scope list).
- Any future revisit of `crates/mcp` itself (its lifecycle surface is
  tracked separately from this RFC).

## Related

- `crates/agent-runtime/src/mcp_oauth.rs` — OAuth implementation
- `crates/agent-runtime/src/mcp.rs` — client, transports, injection sites
- `crates/tui/src/main.rs` — `mcp auth` subcommand
- `docs/MCP.md` — user-facing OAuth documentation
- `crates/mcp/src/lib.rs` — remaining lifecycle/compat layer (unchanged)
- Issue #2190 — this RFC
