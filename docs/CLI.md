# CLI Reference

`codesmith --help` is the canonical list of flags and subcommands. The
common surface:

```bash
codesmith                                         # interactive TUI
codesmith "explain this function"                 # one-shot prompt
codesmith exec --auto --output-format stream-json "fix this bug"  # NDJSON backend stream
codesmith exec --resume <SESSION_ID> "follow up"  # continue a non-interactive session
codesmith --model deepseek-v4-flash "summarize"   # model override
codesmith --model auto "fix this bug"             # auto-select model + thinking
codesmith --yolo                                  # auto-approve tools
codesmith auth set --provider deepseek            # save API key
codesmith doctor                                  # check setup & connectivity
codesmith doctor --json                           # machine-readable diagnostics
codesmith setup --status                          # read-only setup status
codesmith setup --tools --plugins                 # scaffold tool/plugin dirs
codesmith models                                  # list live API models
codesmith sessions                                # list saved sessions
codesmith resume --last                           # resume the most recent session in this workspace
codesmith resume <SESSION_ID>                     # resume a specific session by UUID
codesmith fork <SESSION_ID>                       # fork a saved session into a sibling path
codesmith serve --http                            # HTTP/SSE API server
codesmith serve --mobile                          # LAN mobile control page; token-gated by default
codesmith serve --acp                             # ACP stdio adapter for Zed/custom agents
codesmith run pr <N>                              # fetch PR and pre-seed review prompt
codesmith mcp list                                # list configured MCP servers
codesmith mcp validate                            # validate MCP config/connectivity
codesmith mcp-server                              # run dispatcher MCP stdio server
codesmith update                                  # check for and apply binary updates
codesmith version                                 # print the CLI version (same output as --version)
codesmith docker                                  # print the container quick start
```

## Prompts and subcommands

A bare first word is read as a subcommand name: `codesmith docker` and
`codesmith version` are commands, and an unrecognized word fails with
`unrecognized subcommand '<word>'` (exit code 2) instead of opening the TUI.

Prompts keep their documented forms: `codesmith "explain this function"` (one
quoted argument), `codesmith hello world` (the whole unquoted tail), and
`codesmith -p <PROMPT>` for a single-word prompt such as `codesmith -p docker`.

Without `-p`, a lone ASCII word cannot be told apart from a mistyped command,
so it is rejected with a hint pointing at `-p`. Non-ASCII words
(`codesmith 总结`) stay prompts, since they cannot name a command.

Inside the TUI, `/provider` opens the provider picker and `/model` opens the
local model/thinking picker. `/provider openrouter` and `/model <id>` switch
directly, while `/models` explicitly fetches and lists live API models when the
active provider supports model listing.

Two composer prefixes complement slash commands: `!cmd` runs a shell command
directly in your shell and submits the captured output to the session, and
`:name:` expands an emoji shortcode (with a suggestion popup while typing).
See [KEYBINDINGS.md](KEYBINDINGS.md) for the full composer editing set.

## Sessions: branching and rollback

Saved sessions are intentionally branchable. `codesmith fork <SESSION_ID>` copies
an existing saved session into a new sibling session, records the parent session
id in metadata, and opens that fork so you can explore an alternate direction
without polluting the original path. The session picker and `codesmith sessions`
mark forked sessions with their parent id.

Inside the TUI, Esc-Esc backtrack can rewind the active transcript to a prior
user prompt and put that prompt back in the composer for editing. `/restore`
and `revert_turn` are separate workspace rollback tools: they restore files
from side-git snapshots but do not rewrite conversation history.

More detail: [PRESETS.md — Branching and Rollback](PRESETS.md#branching-and-rollback).

## Docker

Release images are published to GHCR:

```bash
docker volume create codesmith-home

docker run --rm -it \
  -e CODESMITH_API_KEY="$CODESMITH_API_KEY" \
  -v codesmith-home:/home/codesmith/.codesmith \
  -v "$PWD:/workspace" \
  -w /workspace \
  ghcr.io/camilesing/codesmith:latest
```

Pinned tags, local image builds, volume ownership notes, and non-interactive
pipeline usage: [DOCKER.md](DOCKER.md).

`codesmith docker` prints the same run command without needing the TUI binary,
so it also works from a bare container or a CI shell.
