# MCP (External Tool Servers)

codesmith can load additional tools via MCP (Model Context Protocol). MCP servers are either local stdio child processes that the REPL spawns, or remote servers reached over Streamable HTTP (with legacy SSE fallback), SSE, or WebSocket.

Browsing note:
- `web.run` is the canonical built-in browsing tool.
- `web_search` remains available as a compatibility alias for older prompts and integrations.

Server mode note:
- `codesmith serve --mcp` runs the MCP stdio server.
- `codesmith serve --http` runs the runtime HTTP/SSE API (separate mode).
- `codesmith mcp-server` is an equivalent stdio entrypoint.

## Bootstrap MCP Config

Create a starter MCP config at your resolved MCP path:

```bash
codesmith mcp init
```

`codesmith setup --mcp` performs the same MCP bootstrap alongside skills setup.

Common management commands:

```bash
codesmith mcp list
codesmith mcp tools [server]
codesmith mcp add <name> --command "<cmd>" --arg "<arg>"
codesmith mcp add <name> --url "http://localhost:3000/mcp"
codesmith mcp enable <name>
codesmith mcp disable <name>
codesmith mcp remove <name>
codesmith mcp validate
```

## In-REPL Manager

Inside the interactive REPL, `/mcp` opens a compact manager for the resolved
MCP config path. It shows each configured server, whether it is enabled or
disabled, its transport, command or URL, timeout values, connection errors,
and discovered tools/resources/prompts when discovery has been run.

Supported in-REPL actions:

```text
/mcp init
/mcp init --force
/mcp add stdio <name> <command> [args...]
/mcp add http <name> <url>
/mcp enable <name>
/mcp disable <name>
/mcp remove <name>
/mcp validate
/mcp reload
```

`/mcp validate` and `/mcp reload` reconnect for UI discovery and refresh the
manager snapshot. Config edits made from the manager are written immediately, but
the model-visible MCP tool pool is not hot-reloaded; the manager marks this as
restart-required until the REPL is restarted.

## Config File Location

Default path:

- `~/.codesmith/mcp.json`

Overrides:

- Config: `mcp_config_path = "/path/to/mcp.json"`
- Env: `CODESMITH_MCP_CONFIG=/path/to/mcp.json`

`codesmith mcp init` (and `codesmith setup --mcp`) writes to this resolved path.

The interactive `/config` editor also exposes `mcp_config_path`. Changing it in
the REPL updates the path used by `/mcp`, and requires a restart before the
model-visible MCP tool pool is rebuilt.

After editing the file or changing `mcp_config_path`, restart the REPL.

## Tool Naming

Discovered MCP tools are exposed to the model as:

- `mcp__<server>__<tool>`

Example: a server named `git` with a tool named `status` becomes `mcp__git__status`. The older single-underscore form (`mcp_<server>_<tool>`) is still accepted as a call-time alias for backward compatibility, but the double-underscore form is what the model sees in tool listings.

## Resource and Prompt Helpers

The CLI also exposes helper tools when MCP is enabled:

- `list_mcp_resources` (optional `server` filter)
- `list_mcp_resource_templates` (optional `server` filter)
- `mcp_read_resource` / `read_mcp_resource` (aliases)
- `mcp_get_prompt`

## Minimal Example

```json
{
  "timeouts": {
    "connect_timeout": 10,
    "execute_timeout": 60,
    "read_timeout": 120
  },
  "servers": {
    "example": {
      "command": "node",
      "args": ["./path/to/your-mcp-server.js"],
      "env": {},
      "disabled": false
    }
  }
}
```

You can also use `mcpServers` instead of `servers` for compatibility with other clients.

## Running CodeSmith as an MCP Server

You can register your local CodeSmith binary as an MCP server so other CodeSmith sessions (or any MCP client) can call its tools.

### Quick Setup

```bash
codesmith mcp add-self
```

This resolves the current binary path, generates a config entry that runs `codesmith serve --mcp`, and writes it to your MCP config file. The default server name is `codesmith`.

Options:

- `--name <NAME>` — custom server name (default: `codesmith`)
- `--workspace <PATH>` — workspace directory for the server

### Manual Config

Equivalent manual entry in `~/.codesmith/mcp.json`:

```json
{
  "servers": {
    "codesmith": {
      "command": "/path/to/codesmith",
      "args": ["serve", "--mcp"],
      "env": {}
    }
  }
}
```

The `codesmith` binary supports `serve --mcp` directly; `codesmith mcp-server`
is the equivalent dedicated stdio entrypoint. The `mcp add-self` command
automatically resolves the current binary path.

### Prerequisites

- The binary referenced in `command` must exist and be executable.
- The MCP server runs as a child process via stdio — no network ports required.
- Each MCP client session spawns its own server process.

### Tool Naming

Tools from a self-hosted CodeSmith server follow the standard naming convention:

- `mcp__<server>__<tool>` — with the default server name `codesmith`, this is `mcp__codesmith__<tool>`

For example, the `shell` tool becomes `mcp__codesmith__shell`.

### MCP Server vs HTTP/SSE API vs ACP

| | `codesmith serve --mcp` | `codesmith serve --http` | `codesmith serve --acp` |
|---|---|---|---|
| **Protocol** | MCP stdio | HTTP/SSE JSON-RPC | ACP stdio |
| **Use case** | Tool server for MCP clients | Runtime API for apps | Editor agent for Zed/custom ACP clients |
| **Config** | `~/.codesmith/mcp.json` entry | Direct URL connection | Editor `agent_servers` custom command |
| **Lifecycle** | Spawned per client session | Long-running daemon | Spawned per editor agent session |

Use `mcp add-self` when you want CodeSmith tools available to other MCP clients.
Use `serve --http` when building applications that consume the API directly.
Use `serve --acp` when an editor wants to talk to CodeSmith as an ACP agent.

### Verification

After adding, test the connection:

```bash
codesmith mcp validate
codesmith mcp tools codesmith
```

## Server Fields

Per-server settings:

- `command` (string, required for stdio servers): the executable to spawn. Remote servers use `url` instead.
- `args` (array of strings, optional)
- `env` (object, optional)
- `url` (string, optional): base URL of a remote MCP server. URL-based servers use Streamable HTTP by default and fall back to legacy SSE when the server rejects Streamable HTTP.
- `transport` (string, optional): explicit transport override for `url` servers. Supported values: `http` / `streamable` / `streamable-http` (default), `sse`, `sse-ide`, `ws` / `websocket`, `ws-ide` / `websocket-ide`. Use `sse` or `sse-ide` for legacy SSE endpoints that must start with endpoint discovery, and `ws` / `ws-ide` for WebSocket MCP endpoints.
- `headers` (object, optional): extra HTTP headers sent with every request to this server (e.g. `Authorization: Bearer ...`). Only the HTTP transports honor this; stdio servers ignore it. Header keys and values are passed through as-is (no environment-variable substitution) and are stored in plain text in `mcp.json` — treat the file with the same care as any other secret-bearing config.
- `connect_timeout`, `execute_timeout`, `read_timeout` (seconds, optional): see [Remote Transports](#remote-transports-http) for what each one bounds.
- `disabled` (bool, optional)
- `enabled` (bool, optional, default `true`)
- `required` (bool, optional): startup/connect validation fails if this server cannot initialize.
- `enabled_tools` (array, optional): allowlist of tool names for this server.
- `disabled_tools` (array, optional): denylist applied after `enabled_tools`.
- `oauth` (object, optional): OAuth device-flow configuration for URL servers; see [OAuth](#oauth) below.

## Remote Transports (HTTP)

A `url` server entry is reached over MCP Streamable HTTP unless `transport`
says otherwise. A complete remote entry:

```json
{
  "servers": {
    "docs": {
      "url": "https://mcp.example.com/mcp",
      "headers": { "Authorization": "Bearer <token>" },
      "connect_timeout": 10,
      "execute_timeout": 60,
      "read_timeout": 120
    }
  }
}
```

### Streamable HTTP

- Every JSON-RPC message is one `POST` to the URL with
  `Accept: application/json, text/event-stream`. The server answers with a
  plain JSON body or an SSE stream; responses to notifications are `202`.
- SSE responses are decoded **incrementally**: events become available as
  bytes arrive — split or coalesced TCP segments and several events per
  stream are reassembled — so a server that streams progress notifications
  before the final result does not stall the call until the body ends.
- **Session ID.** The `Mcp-Session-Id` the server returns on the
  `initialize` response (or on a best-effort `GET` preflight, for servers
  that demand a session on every request) is captured and replayed as a
  header on every later request.
- **Legacy SSE fallback.** If the server rejects the `POST` with
  404/405/406/415/501, the client switches to the legacy SSE transport
  (endpoint discovery over a long-lived `GET`, messages `POST`ed to the
  discovered endpoint). Set `transport = "sse"` to skip the negotiation for
  servers that only speak legacy SSE, or `"ws"` for WebSocket endpoints.

### Timeouts

- `connect_timeout` bounds the TCP connect and the handshake
  (`initialize` + tool/resource/prompt discovery), and the GET + endpoint
  discovery phase of legacy SSE.
- `execute_timeout` bounds each tool call end to end (request sent and
  response received).
- `read_timeout` bounds the idle gap between bytes on any HTTP stream —
  the Streamable HTTP response stream and the legacy SSE `GET` stream
  alike. A stream that keeps delivering bytes can outlive `connect_timeout`
  freely; a peer that goes silent is cut after `read_timeout`.

### Disconnects and retries

- **Expired session** (HTTP 404, JSON-RPC error `-32001`, or an
  "expired/invalid session" body): the cached session ID is dropped, the
  connection is rebuilt, and the tool call is retried **once**.
- **Long-lived stream cut** (legacy SSE `GET` or WebSocket closed): same
  single reconnect-and-retry on the next call. Nothing reconnects in the
  background; reconnection happens lazily on the next use.
- **Streamable HTTP stream ended without the response**: the call fails
  with a diagnosable error and is **not** retried — the server already
  received the request and may have run the tool, so an automatic replay
  could double a side effect. Re-issue the call if that is safe.
- **401**: one credential refresh (stored OAuth token, or the Claude.ai
  refresh command) and one retry. Persistent auth failures are cached for
  15 minutes so the REPL shows a login hint instead of re-trying every
  turn.

### Limitations

- No standalone server-to-client `GET` stream is opened: server-initiated
  requests (sampling, roots, …) are not supported. Notifications that
  arrive on a response stream are decoded and ignored.
- Idle streams are cut after `read_timeout`; a server that holds a
  response stream open without SSE keep-alives for longer than that sees a
  reconnect on the next call.
- A response truncated mid-event is parsed leniently: a cut tail surfaces
  as a JSON-RPC decode error rather than a dedicated truncation error.

## OAuth

URL-based MCP servers that require user OAuth (e.g. GitHub) can be authorized
with a device flow instead of pasting a long-lived token into `headers`.

Add an `oauth` table to the server entry in `mcp.json`:

```jsonc
{
  "mcpServers": {
    "github": {
      "url": "https://api.githubcopilot.com/mcp/",
      "oauth": {
        "provider": "github",
        "client_id": "<your OAuth app client id>",
        "scopes": ["repo"]
      }
    }
  }
}
```

Fields:

- `provider` (string, required): `github` fills the standard GitHub device-code
  and token endpoints. Any other value is treated as a custom provider and
  requires explicit `device_code_url` and `token_url`.
- `client_id` (string, required).
- `scopes` (array of strings, optional): space-joined into the device-flow
  `scope` parameter.
- `device_code_url` / `token_url` (string, optional): custom endpoints for
  non-GitHub providers.

Then authorize once:

```console
$ codesmith mcp auth github
Authorization required for MCP server `github` (github).
  1. Open:  https://github.com/login/device
  2. Code:  ABCD-1234   (expires in 900s)
Waiting for authorization…
Authorization stored for MCP server `github`.
```

Behavior:

- The token is stored via `codesmith-secrets` — the system keyring when
  available, otherwise the permissioned-file fallback under
  `~/.codesmith/secrets/`. It never lands in `mcp.json`.
- On connect, the stored token is injected as the `Authorization: Bearer`
  header unless `headers` already carries one. The HTTP transports refresh it
  once via the refresh-token grant when the server answers 401. A
  user-configured `headers.Authorization` always wins and is never overwritten.
- Stdio servers ignore `oauth` — pass credentials via their `env` table.
- Out of scope: PKCE/redirect flows and client-credentials grants.

## Feature Flag

MCP support is gated by the `mcp` feature flag, enabled by default (experimental). To turn MCP off entirely, set in `config.toml`:

```toml
[features]
mcp = false
```

## Safety Notes

MCP tools now flow through the same tool-approval framework as built-in tools. Read-only MCP helpers (resource/prompt listing and reads) can run without prompts in suggestive approval modes, while side-effectful MCP tools require approval.

You should still only configure MCP servers you trust, and treat MCP server configuration as equivalent to running code on your machine.

## Troubleshooting

- Run `codesmith doctor` to confirm the MCP config path it resolved and whether it exists.
- In the REPL, run `/mcp validate` to refresh the visible server/tool snapshot.
- If the MCP config is missing, run `codesmith mcp init --force` to regenerate it.
- If tools don’t appear, verify the server command works from your shell and that the server supports MCP `tools/list`.
