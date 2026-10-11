# CLI Reference

`codesmith --help` is the canonical list of flags and subcommands. The
common surface:

```bash
codesmith                                         # interactive REPL
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
codesmith --skip-onboarding                        # bypass first-run onboarding questions
codesmith --continue                               # continue the most recent session in this workspace
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
`unrecognized subcommand '<word>'` (exit code 2) instead of starting the REPL.

Prompts keep their documented forms: `codesmith "explain this function"` (one
quoted argument), `codesmith hello world` (the whole unquoted tail), and
`codesmith -p <PROMPT>` for a single-word prompt such as `codesmith -p docker`.
A `--` terminator ends subcommand matching too, so `codesmith -- docker` also
sends `docker` as the prompt.

Without `-p`, a lone ASCII word cannot be told apart from a mistyped command,
so it is rejected with a hint pointing at `-p`. Non-ASCII words
(`codesmith 总结`) stay prompts, since they cannot name a command.

## First-run onboarding

A bare `codesmith` on a machine without `~/.codesmith/.onboarded` runs
onboarding as plain CLI questions before the REPL starts: language → provider
→ endpoint URL (OpenAI/Anthropic routes only) → API key → workspace trust.
Answers land in the same durable state the former TUI wizard wrote
(`~/.codesmith/config.toml`, the settings locale, the OS keyring, and the
trust table). Existing credentials — a provider env var, a saved key, or a
self-hosted runtime such as Ollama — skip the key question, and declining the
trust prompt exits without writing anything. Pass `--skip-onboarding` to
bypass the questions entirely.

Inside the REPL, two gates can still interrupt a later session: a missing
credential (for example after `/logout`) re-opens the key screen, and a first
visit to an untrusted directory re-opens the trust prompt.

Inside the REPL, `/provider` opens the provider picker and `/model` opens the
local model/thinking picker. `/provider openrouter` and `/model <id>` switch
directly, while `/models` explicitly fetches and lists live API models when the
active provider supports model listing.

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
