# MCP (External Tool Servers)

codesmith can load additional tools via MCP (Model Context Protocol). MCP servers are either local stdio child processes that the TUI spawns, or remote servers reached over Streamable HTTP (with legacy SSE fallback), SSE, or WebSocket.

Browsing note:
- `web.run` is the canonical built-in browsing tool.
- `web_search` remains available as a compatibility alias for older prompts and integrations.

Server mode note:
- `codesmith-tui serve --mcp` runs the MCP stdio server.
- `codesmith-tui serve --http` runs the runtime HTTP/SSE API (separate mode).
- The `codesmith` dispatcher exposes `codesmith mcp-server` as an equivalent stdio
  entrypoint used by the split CLI.

## Bootstrap MCP Config

Create a starter MCP config at your resolved MCP path:

```bash
codesmith-tui mcp init
```

`codesmith-tui setup --mcp` performs the same MCP bootstrap alongside skills setup.

Common management commands:

```bash
codesmith-tui mcp list
codesmith-tui mcp tools [server]
codesmith-tui mcp add <name> --command "<cmd>" --arg "<arg>"
codesmith-tui mcp add <name> --url "http://localhost:3000/mcp"
codesmith-tui mcp enable <name>
codesmith-tui mcp disable <name>
codesmith-tui mcp remove <name>
codesmith-tui mcp validate
```

## In-TUI Manager

Inside the interactive TUI, `/mcp` opens a compact manager for the resolved
MCP config path. It shows each configured server, whether it is enabled or
disabled, its transport, command or URL, timeout values, connection errors,
and discovered tools/resources/prompts when discovery has been run.

Supported in-TUI actions:

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
manager snapshot. Config edits made from the TUI are written immediately, but
the model-visible MCP tool pool is not hot-reloaded; the manager marks this as
restart-required until the TUI is restarted.

## Config File Location

Default path:

- `~/.codesmith/mcp.json` (`~/.deepseek/mcp.json` is still read when the CodeSmith file is absent)

Overrides:

- Config: `mcp_config_path = "/path/to/mcp.json"`
- Env: `CODESMITH_MCP_CONFIG=/path/to/mcp.json` (legacy aliases `CODEWHALE_MCP_CONFIG` and `DEEPSEEK_MCP_CONFIG` are still accepted)

`codesmith-tui mcp init` (and `codesmith-tui setup --mcp`) writes to this resolved path.

The interactive `/config` editor also exposes `mcp_config_path`. Changing it in
the TUI updates the path used by `/mcp`, and requires a restart before the
model-visible MCP tool pool is rebuilt.

After editing the file or changing `mcp_config_path`, restart the TUI.

## Tool Naming

Discovered MCP tools are exposed to the model as:

- `mcp__<server>__<tool>`

Example: a server named `git` with a tool named `status` becomes `mcp__git__status`. The older single-underscore form (`mcp_<server>_<tool>`) is still accepted as a call-time alias for backward compatibility, but the double-underscore form is what the model sees in tool listings.

The command palette includes MCP entries grouped by server. It shows disabled
and failed servers instead of hiding them, and uses the same runtime tool names
shown to the model.

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
codesmith-tui mcp add-self
```

This resolves the current binary path, generates a config entry that runs `codesmith-tui serve --mcp`, and writes it to your MCP config file. The default server name is `codesmith`.

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

The `codesmith-tui` binary supports `serve --mcp` directly. The `codesmith`
dispatcher offers the equivalent `codesmith mcp-server` stdio entrypoint. Use
whichever is on your `PATH` (run `which codesmith` or `which codesmith-tui` to
find the full path). The `mcp add-self` command automatically resolves the
correct binary.

### Prerequisites

- The binary referenced in `command` must exist and be executable.
- The MCP server runs as a child process via stdio — no network ports required.
- Each MCP client session spawns its own server process.

### Tool Naming

Tools from a self-hosted CodeSmith server follow the standard naming convention:

- `mcp__<server>__<tool>` — with the default server name `codesmith`, this is `mcp__codesmith__<tool>`

For example, the `shell` tool becomes `mcp__codesmith__shell`.

### MCP Server vs HTTP/SSE API vs ACP

| | `codesmith-tui serve --mcp` | `codesmith-tui serve --http` | `codesmith-tui serve --acp` |
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
codesmith-tui mcp validate
codesmith-tui mcp tools codesmith
```

## Server Fields

Per-server settings:

- `command` (string, required for stdio servers): the executable to spawn. Remote servers use `url` instead.
- `args` (array of strings, optional)
- `env` (object, optional)
- `url` (string, optional): base URL of a remote MCP server. URL-based servers use Streamable HTTP by default and fall back to legacy SSE when the server rejects Streamable HTTP.
- `transport` (string, optional): explicit transport override for `url` servers. Supported values: `http` / `streamable` / `streamable-http` (default), `sse`, `sse-ide`, `ws` / `websocket`, `ws-ide` / `websocket-ide`. Use `sse` or `sse-ide` for legacy SSE endpoints that must start with endpoint discovery, and `ws` / `ws-ide` for WebSocket MCP endpoints.
- `headers` (object, optional): extra HTTP headers sent with every request to this server (e.g. `Authorization: Bearer ...`). Only the HTTP transports honor this; stdio servers ignore it. Header keys and values are passed through as-is (no environment-variable substitution) and are stored in plain text in `mcp.json` — treat the file with the same care as any other secret-bearing config.
- `connect_timeout`, `execute_timeout`, `read_timeout` (seconds, optional)
- `disabled` (bool, optional)
- `enabled` (bool, optional, default `true`)
- `required` (bool, optional): startup/connect validation fails if this server cannot initialize.
- `enabled_tools` (array, optional): allowlist of tool names for this server.
- `disabled_tools` (array, optional): denylist applied after `enabled_tools`.

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

- Run `codesmith-tui doctor` to confirm the MCP config path it resolved and whether it exists.
- In the TUI, run `/mcp validate` to refresh the visible server/tool snapshot.
- If the MCP config is missing, run `codesmith-tui mcp init --force` to regenerate it.
- If tools don’t appear, verify the server command works from your shell and that the server supports MCP `tools/list`.
