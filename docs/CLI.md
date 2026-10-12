# CLI Reference

`codesmith --help` is the canonical list of flags and subcommands. The
common surface:

```bash
codesmith                                         # interactive REPL
codesmith -p "explain this function"              # open the REPL with this prompt submitted
codesmith exec "explain this function"            # one-shot, non-interactive prompt
codesmith exec --auto --output-format stream-json "fix this bug"  # NDJSON backend stream
codesmith exec --resume <SESSION_ID> "follow up"  # continue a non-interactive session
codesmith --yolo                                  # auto-approve tools
codesmith auth set --provider deepseek            # save API key
codesmith doctor                                  # check setup & connectivity
codesmith doctor --json                           # machine-readable diagnostics
codesmith setup --status                          # read-only setup status
codesmith setup --tools --plugins                 # scaffold tool/plugin dirs
codesmith models                                  # list live API models
codesmith sessions                                # list saved sessions
codesmith --skip-onboarding                        # bypass first-run onboarding questions
codesmith --continue                               # continue the most recent session in this workspace
codesmith resume --last                           # resume the most recent session in this workspace
codesmith resume <SESSION_ID>                     # resume a specific session by UUID
codesmith fork <SESSION_ID>                       # fork a saved session into a sibling path
codesmith serve --http                            # HTTP/SSE API server
codesmith serve --mobile                          # LAN mobile control page; token-gated by default
codesmith serve --acp                             # ACP stdio adapter for Zed/custom agents
codesmith pr <N>                                  # fetch PR and pre-seed review prompt
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
`unrecognized subcommand '<word>'` (exit code 2) and a tip pointing at `-p`.
There is no top-level positional prompt: quoting (`codesmith "explain this
function"`), an unquoted tail, a `--` terminator, and non-ASCII bare words
(`codesmith 总结`) all fail the same way.

The prompt entry points are:

- `codesmith -p <PROMPT>` — takes one or more values (joined into the initial
  prompt), starts the interactive REPL, and submits the prompt as the first
  turn. It is not one-shot; the session stays open for follow-ups.
- `codesmith exec "<prompt>"` — the non-interactive one-shot form.

Inside the REPL, `/help` is the live list of slash commands — typing `/`
reports how many exist, `Tab` completes the name (press it twice to list the
matches), and the dim text after the cursor previews what the match does. `/plan`,
`/agent`, and `/yolo` are shortcuts for the matching `/mode` value — or cycle
them with `Shift+Tab` at the prompt (`agent → plan → yolo`, plus `coordinator`
while the experimental `coordinator_mode` feature is enabled).

## Model selection

The top-level `--model <id>` flag overrides the model for that run — it
folds into the same slot as the `CODESMITH_MODEL` env handoff (the root
`model` slot for DeepSeek, the provider-scoped slot for other providers),
so it wins over config.toml and satisfies the custom-gateway explicit-model
check. Other ways to select a model:

- `model = <id>` under `[providers.<id>]` in `~/.codesmith/config.toml`
- the `CODESMITH_MODEL` environment variable
- `/model <id>` in the REPL (persisted per provider across restarts)
- `codesmith exec --model <id>` for a single non-interactive run

`codesmith models` lists live models from the configured endpoint, while
`codesmith model list` and `codesmith model resolve` inspect the built-in
registry and the resolved selection.

## First-run onboarding

A bare `codesmith` on a machine without the `~/.codesmith/.onboarded`
marker asks plain CLI questions on stdin — language, provider, custom
endpoint (for gateway providers), API key, and directory trust — then
writes the answers to `config.toml`, the OS keyring, and the language
setting, records the marker, and starts the REPL. A start that already
has a credential — a provider env var, a saved key, or a self-hosted
runtime such as Ollama — skips the key question; the other questions are
still asked on the bare invocation. Non-terminal stdin never triggers
the questions (a credentialed non-terminal start silently writes the
marker instead), and subcommands never ask: a missing credential there
fails the first turn with a provider auth error. The API key is echoed
in cleartext, and Moonshot Kimi OAuth is not offered in the flow — use
`codesmith login` for it. `--skip-onboarding` bypasses the questions.

`/logout` removes the API key and the onboarding marker, so the next
bare `codesmith` re-runs the questions; `codesmith auth set --provider
<id>` (prompts for the key) and `codesmith login` configure a key
without the flow. After the first run, trust only gates whether
project-level config and `.env` files are loaded, and `/trust` manages
it.

Inside the REPL, a bare `/provider` prints the current provider and
`/provider <id>` switches (session-scoped; a restart hint is printed, since a
full re-config needs a new session). A bare `/model` prints the current model
and `/model <id>` switches and persists it. `/models` fetches and lists the
live models from the active endpoint (`--json` for pretty JSON; providers
without a `/models` catalog report an empty list) — the same listing as the
`codesmith models` subcommand.

## Sessions: branching and rollback

Saved sessions are intentionally branchable. `codesmith fork <SESSION_ID>` copies
an existing saved session into a new sibling session, records the parent session
id in metadata, and opens that fork so you can explore an alternate direction
without polluting the original path. The session picker and `codesmith sessions`
mark forked sessions with their parent id.

Inside the REPL, `/undo` drops the last exchange from the transcript and
`/retry` drops it and re-sends the same user message. `revert_turn` is a
separate workspace rollback tool: it restores files
from side-git snapshots but does not rewrite conversation history.

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

`codesmith docker` prints the same run command without starting the REPL,
so it also works from a bare container or a CI shell.
