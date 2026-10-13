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

Keyboard and interaction details — prompt keys, the mid-turn composer, vim
mode, shell mode — live in [KEYBINDINGS.md](KEYBINDINGS.md); `?` on an empty
prompt prints the same table.

Beyond the core command set, the REPL carries the Claude Code interactions
this fork's users asked for. The ones worth knowing:

- **While a turn runs you can type.** `Enter` queues, `Ctrl+X Ctrl+S` sends
  now, `Esc` backgrounds running shell/sub-agent work or interrupts, `Ctrl+B`
  backgrounds a shell, `Ctrl+X Ctrl+K` stops all background work. A queued
  message reaches the model when the tool call it waited on finishes.
- **`!command`** runs in your shell and lets the model answer from its output
  (`respond_to_shell_commands = false` records it without a turn).
- **`/btw <question>`** answers from the session's own conversation, with no
  tools, and does not add the exchange to it (it is recorded to
  `~/.codesmith/side-questions.jsonl`).
- **`/rewind [n|list]`** restores the workspace to one of the file snapshots
  the engine takes around file-mutating turns. Files only: `/undo` and
  `/retry` are the conversation half.
- **`/code-review`, `/security-review`, `/simplify`, `/run`, `/verify`** are
  bundled skills (prompt workflows), listed by `/skills` and overridable by a
  skill of the same name on disk.
- **`/effort`, `/autocompact`, `/usage`, `/tasks`, `/plugins`,
  `/reload-skills`, `/reload-plugins`, `/theme`, `/color`, `/statusline`,
  `/copy`, `/doctor`, `/debug`, `/release-notes`, `/terminal-setup`,
  `/keybindings`** cover the model tier, the background view, the extension
  surfaces, and the appearance/diagnostic odds and ends. `/permissions`
  shows what runs without asking — session grants, persisted grants, trusted
  paths, and the `[sandbox] auto_allow` rules — and writes the persisted
  grant store (`/permissions allow "git status"` covers `git status -s` and
  not `git push`, using the same arity dictionary the approval prompt uses). `/recap` prints a
  one-line summary on demand, `/output-style` switches the compression style
  CodeSmith has (`is_simple`), `/resume` accepts a session *name*, and
  `/rename` with no argument names the session from its first message.

### Renamed commands

Four names mean what Claude Code means by them now, and the old behaviour is
still reachable:

| Before | Now |
| :- | :- |
| `/verify` (claim-check ledger stats) | `/verify` runs and observes the program; the ledger stats are `/claim-check` |
| `/review <target>` (activated the review skill) | `/review` is an alias of `/code-review`; the skill answers to `/review-skill` or `/skill review <target>` |
| `/undo` (dropped the last exchange) | unchanged — it is the conversation half of the pair `/rewind` (files) completes |
| `/stats` (alias of `/home`) | `/stats` is an alias of `/usage`; `/home` keeps `/overview` |
| `/tasks` (alias of `/task`) | `/tasks` is the background view (jobs + sub-agents + durable tasks); `/task` keeps `add\|list\|show\|cancel` |

### Not implemented

Commands that only exist inside Anthropic's own cloud or account surface have
no local equivalent and are deliberately not registered, so `/help` never
offers something that cannot work: `/desktop` `/mobile` `/teleport`
`/remote-control` `/remote-env` `/background` (background *sessions*)
`/artifacts` `/design` `/design-login` `/design-sync` `/slides` `/voice`
`/chrome` `/claude-in-chrome` `/ide` `/upgrade` `/passes` `/usage-credits`
`/rate-limit-options` `/privacy-settings` `/web-setup` `/install-github-app`
`/install-slack-app` `/autofix-pr` `/ultrareview` `/heapdump` `/radio`
`/stickers` `/powerup` `/team-onboarding` `/insights` `/fast` `/scroll-speed`
`/tui` `/focus` `/keybindings-file` `/list-agents` `/peers` `/schedule`
`/loop` `/batch` `/subtask` `/branch` `/import` `/add-dir` `/cd` `/pr`
`/simplify` variants beyond the bundled skill, and the `/plugin` authoring
surface.

Also out of scope, with the reason: a keybindings configuration file (the
bindings are compiled in; `/keybindings` is read-only), emoji shortcodes and
spell checking (both need data tables this REPL does not ship), and the
fullscreen renderer's transcript-viewer keys.

`codesmith pr <N>` is the PR-review door from the CLI; the REPL does not
carry a `/pr` because the command needs the parsed CLI handle the REPL does
not hold.

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
