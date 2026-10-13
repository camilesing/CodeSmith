# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

History before this fork — including the full deepseek-tui / CodeWhale era up
to v0.8.48 — is archived in [docs/legacy/CHANGELOG-upstream.md](docs/legacy/CHANGELOG-upstream.md).
See [docs/HISTORY.md](docs/HISTORY.md) for the project lineage.

## [Unreleased]

### Added

- **The REPL gained Claude Code's keyboard and command surface** — the
  interactions this fork's users asked for, ported to a line-based frontend
  rather than a fullscreen one. The complete key table is
  [docs/KEYBINDINGS.md](docs/KEYBINDINGS.md) (`?` on an empty prompt prints it,
  `/keybindings` reprints it).

  Keys. A mid-turn composer: while a turn runs you can type, edit, and queue —
  `Enter` queues, `Ctrl+X Ctrl+S` (or `Ctrl+Enter` where the terminal reports
  it) sends now, `Esc` backgrounds running shell/sub-agent work or interrupts,
  `Ctrl+B` backgrounds a shell, `Ctrl+X Ctrl+K` stops all background work
  behind a 3-second confirm, `Ctrl+T` toggles the model's task checklist, and
  `Ctrl+O` works at the prompt as well as mid-turn. A queued message reaches
  the model as soon as the tool call it waited on finishes, through the
  engine's existing steer path, so the session log keeps agreeing with what
  the model saw. Anything still in the composer when a turn ends returns to
  the next prompt, and `Up` takes queued messages back for editing. At the
  prompt: `Ctrl+S` stashes a draft, `Ctrl+D` deletes forward or quits on a
  second press, `Esc Esc` clears a draft while keeping it for `Up`, `Ctrl+G`
  opens `$VISUAL`/`$EDITOR`, `Alt+P`/`Alt+T` switch model and thinking, `?`
  prints the shortcut panel, a trailing `\` or `Ctrl+J` makes input
  multi-line, and history is now per workspace. `composer_vim_mode`,
  `max_input_history`, `bracketed_paste`, `mention_walk_depth`, and
  `mention_menu_limit` were documented settings that nothing read; they are
  wired now (`/config editor_mode vim` switches to rustyline's vi keymap).

  Commands. Shell mode (`!command`) runs in your own shell and lets the model
  answer from the output; `/btw` asks about the session without adding to it
  (recorded to `~/.codesmith/side-questions.jsonl`); `/rewind` lists and
  restores the file snapshots the engine already takes; `/code-review`,
  `/security-review`, `/simplify`, `/run`, and `/verify` are bundled skills;
  `/effort`, `/autocompact`, `/usage`, `/tasks`, `/plugins`,
  `/reload-skills`, `/reload-plugins`, `/theme`, `/color`, `/statusline`,
  `/copy`, `/doctor`, `/debug`, `/release-notes`, `/terminal-setup`, and
  `/keybindings` cover the model tier, the background view, the extension
  surfaces, and the diagnostic odds and ends. `@path` completion and
  mid-prompt command completion arrive with them.

  Renames, so four names mean what Claude Code means by them: `/verify` now
  runs and observes the program (the claim-check ledger stats moved to
  `/claim-check`); `/review` is an alias of `/code-review` (the installed
  review skill answers to `/review-skill` or `/skill review <target>`);
  `/stats` is an alias of `/usage` (`/home` keeps `/overview`); `/tasks` is
  the background view (`/task` keeps its subcommands). `/undo` is unchanged
  and is the conversation half of the pair `/rewind` completes for files.

  Deliberately absent: the commands that only exist inside Anthropic's cloud
  or account surface have no local equivalent and are not registered, so
  `/help` never offers something that cannot work — the list, with reasons, is
  in [docs/CLI.md](docs/CLI.md).

### Changed

- `[sandbox] auto_allow` is read: the key was documented with matching
  semantics (and a prefix dictionary with tests) but no parser, so a rule in
  the file did nothing. Its entries now become approval grants at startup,
  and `/permissions` shows the whole picture — session grants, persisted
  grants in `~/.codesmith/approvals.toml`, trusted paths, and the config
  rules — while `/permissions allow|deny|remove` edits it. The grant store
  had readers and no writer; it has one now.
- `/memory auto [on|off]` reports the effective auto-memory state and names
  the level deciding it (environment first, then `[memory] enabled`), and
  writes that key when asked to change it.
- Shell-mode completion: inside a `!` line, `Tab` completes workspace paths
  for a word with a separator and the project's own `!` history otherwise.
- `/import [claude|cursor|codex|gemini] [--yes]` copies another agent CLI's
  MCP servers (stdio and URL alike, headers included) into
  `~/.codesmith/mcp.json`, and reports the surfaces it does not need to touch
  because CodeSmith already reads them natively.
- `/cd` and `/add-dir`: the session can move its working directory (only into
  the workspace's own subtree or a path the trust record covers — the exact
  `/trust add` line is printed otherwise) and add one to that record.
- Input history is per workspace (`~/.codesmith/history/<workspace>`) and its
  default cap is 500 entries; the pre-upgrade single file is read once for a
  workspace that has none of its own and left in place.
- `auto_compact` is reachable from `/config` as documented (the key had no
  handler) and `/autocompact` pins the window for a session.
- The REPL's known-limitations note in `state.rs` was rewritten: it had been
  listing capabilities the port just built (single-line input, no mid-turn
  input, no `@` completion) as absent.

- **An out-of-workspace path is now a question, not a dead end**: a file tool
  pointed outside the workspace (`read_file`, `write_file`, `edit_file`,
  `list_dir`, `grep_files`, `file_search`) used to fail with
  `path escapes workspace` and nothing else — the model reported the refusal
  and stopped, and the only exit (`/trust add <path>`, a REPL command the model
  cannot run) never reached the user. The engine now emits
  `Event::PathAccessRequired`, and the host asks:

  ```text
  ⚠ path outside the workspace: /abs/sibling-repo (list_dir)
  allow? [y] allow it now · [a] always allow it in this workspace (saved) · [n] skip:
  ```

  `[y]` grants the path for the turn, `[a]` also persists it to
  `~/.codesmith/workspace-trust.json` — the same record `/trust add` writes and
  `/trust remove` undoes — and `[n]` keeps the refusal, with the model told the
  path was declined instead of `/trust add` being re-offered. The grant reaches
  the call that asked in the same turn: `ToolContext::trusted_external_paths` is
  now a shared live list, so it is visible to the running tool and to any child
  sharing the context. Sub-agents ask through the same channel (their question
  appears in the session that spawned them) via a new id-routing
  `ApprovalRouter`, which also fixes a latent hazard: the old
  drop-on-mismatch wait would let one waiter consume another's answer and leave
  it waiting until cancellation. Headless hosts answer as they do for
  approvals — `--auto` allows the path, otherwise it is refused — so a
  non-interactive run never blocks on a prompt it cannot show.
- **Plan mode can open read-only sub-agents, including one per repository**:
  `prompts/modes/plan.md` has always told the model to "spawn read-only
  sub-agents for parallel investigation", but the Plan tool registry never
  carried `agent_open`/`agent_eval`/`agent_close`, so the model literally could
  not — it read the codebase serially while the prompt promised fan-out. Plan
  mode now registers the family with **every** child narrowed to the read-only
  tools of its own registry (a child cannot write, patch, or shell out,
  whatever type it was opened with), and an `agent_open` `cwd` may now point
  into a directory trusted with `/trust add <path>`, which is what makes
  "one explorer per repository" work — the child's workspace root becomes that
  directory. Untrusted paths are refused with the exact `/trust add` line to
  run. `agent_open` remains approval-gated: two spawns in one message prompt
  once (`[a]` covers the same call shape) and then run concurrently, each
  returning immediately. See `docs/SUBAGENTS.md` ("Read-only roles and Plan
  mode", "Working across repositories").

- **Provider `display_model` and `context_window` overrides**: `[providers.<id>]`
  tables and `[[providers.custom]]` entries accept `display_model = "..."`,
  printed verbatim by the REPL banner, status line, `/status`, `/model`,
  `/tokens`, and `/home`, and `context_window = N`, which drives the `🧭`
  denominator, `/context`, `/tokens`, `doctor --json`'s
  `capability.context_window`, and the auto-compaction trigger. Both exist for
  gateway deployments whose backend model and window the static model-name
  tables cannot see; without them behaviour is unchanged. `display_model` is
  presentation only — requests, session metadata, and logs keep the real model
  id.
- **Honest model provenance in the status line**: the label no longer claims a
  server-reported name it never had. Every provider synthesizes the
  `MessageStart` frame locally from the request id
  (`crates/providers/src/rig_adapter/stream.rs`), so the field the label and the
  session metadata carried was repeating the requested id; the field comments,
  `docs/GUIDE.md`, and the session-metadata doc now say so, and `display_model`
  is the supported way to label a gateway alias.
- **REPL slash-command completion**: typing `/` in the line REPL is no longer
  silent. A dim hint after the cursor previews the matching command (its
  remaining characters plus a one-line description) or reports how many
  commands match; one `Tab` completes to the longest common prefix and a
  second lists the candidates. Names come from a new
  command registry that also drives `/help`, which had fallen behind the
  dispatcher (20 of the 75 dispatchable names). Commands from
  `.codesmith/commands/*.md` load at REPL start, are completed too, and are
  listed by `/help`. Aliases dispatch but are not offered, and only closed
  argument vocabularies (`/mode`, `/provider`, `/verbose`) are completed —
  live model names, sessions, skills, presets, and `@file` paths need IO or
  the network and are not.
- **`/plan` and `/agent`**: restored as `/mode` shorthands. Upstream #1247
  kept `/plan`, `/agent`, and `/yolo` as compatibility shortcuts; the port to
  the line REPL carried over only `/yolo`, so `/plan` answered
  `unknown command`.
- **`Shift+Tab` cycles the run mode in the REPL**: pressing it at the prompt
  walks `agent → plan → yolo` and prints where it landed, with `coordinator`
  as a fourth stop only while the experimental `coordinator_mode` feature is
  on — the gate the startup path already applies, so an unopted user cannot
  fall into the orchestrator-only toolset by pressing a key once too often
  (`/mode coordinator` remains the ungated door). The binding replaces
  rustyline's default `Cmd::CompleteBackward` for BackTab, so Tab completion
  keeps its forward behaviour and loses only the reverse candidate walk. The
  mode rides `Op::SendMessage`, so the switch applies to the next submitted
  turn, and it is unavailable mid-turn, where the key tap reports `Ctrl+O`
  only. `docs/GUIDE.md` and `docs/CLI.md` document the binding.

- **`codesmith version` and `codesmith docker`**: the two commands users type
  expecting output now exist. `version` prints byte-identical output to
  `--version` (same `CODESMITH_BUILD_VERSION`), and `docker` prints the
  container quick start from `docs/DOCKER.md` plus a link to it. Both run in
  the dispatcher itself — no `codesmith-tui` spawn, no config dependency — so
  they work in a plain container or CI shell.
- **Evolution event log + `/verify stats` (P3-9 step 1)**: the
  continuous-evolution machinery is now observable. Every result-claim
  verdict is appended to `~/.codesmith/evolution/verdicts.jsonl`
  (rfc3339 timestamp + the four-element verdict fields + the claiming
  model; `CODESMITH_HOME` redirects it like every state path). Writing is
  best-effort — resolution or IO failure logs a WARN and never breaks the
  engine — and rides the `[verification] result_claims` switch, so the
  feature's own gate is the log's gate. The new `/verify [stats]` command
  aggregates the log into the first evaluation surface: verdict totals,
  the claim-match rate over checkable claims (of the claims that had a
  command behind them, how many survived the re-run), and the
  failure-type histogram. Malformed tail lines from a mid-append crash
  are skipped on load. Doctor analyses and memory-consolidation runs are
  the next event kinds to join the log.
- **Memory consolidation — "sleep learning" (P3-8 step 3)**:
  `codesmith memory consolidate [--apply] [--deterministic-only]` gives
  the KoD memory index an offline maintenance pass (the online loop only
  ever appends). Deterministic passes always run first over
  `MEMORY.md`: exact-duplicate pointer lines removed, pointers to
  deleted topic files dropped, orphaned topic files reported (never
  deleted), and the entrypoint budget (200 lines / 25 KB) checked —
  results land in the new `agent-runtime::knowledge::curator` module
  with its own validator. Unless `--deterministic-only`, the cleaned
  index plus every topic file's frontmatter goes to one advisory LLM
  call (`[utility_model]` when configured, else the main client) which
  proposes a consolidated rewrite (sharper descriptions, grouped
  sections, one pointer per file); the proposal is accepted only if
  `validate_proposed_index` confirms every existing topic file stays
  referenced exactly once with no unknown files and no budget breach —
  otherwise the deterministic result stands. Default is a dry run
  printing a unified diff; `--apply` writes after taking a
  `MEMORY.md.bak` backup. Topic file contents are never touched. Also
  adds the `codesmith memory` CLI passthrough.
- **Doctor LLM fallback layer (P3-8 step 2)**: after `codesmith doctor`'s
  deterministic checks complete, the warnings/errors they collected
  (recorded at the same branches that print them — update checks,
  config.toml presence, provider key, API connectivity, strict_tool_mode
  fallback, MCP servers/config, missing skills dirs, Python/Node/
  pdftotext dependencies, platform sandbox) are handed to one advisory
  LLM call — the `[utility_model]` when configured, else the main client
  — which proposes root causes (preferring explanations that connect
  multiple findings) and one concrete next action per finding, skipping
  findings whose built-in hint already covers them. The section renders
  after "All checks complete!" and is explicitly advisory: deterministic
  results take precedence, the model never executes anything, and every
  skip condition (no resolvable client, empty response, transport error,
  30s timeout) degrades to a quiet `·` line. Usage flows through the
  `cost_status` side-channel. `--json` mode is untouched (CI-safe).
  Configure with `[doctor] llm_fallback` (bool, default `true`). Unit
  tests in `doctor_llm.rs` cover the collector, prompt-safety contract,
  and client round-trip (mock).
- **Result claim verifier (P3-8)**: the first piece of the
  continuous-evolution loop — a result validator for the online turn.
  When a completed turn's final assistant message asserts "tests pass" /
  "build succeeds" (Chinese or English phrase matching, which keeps
  negations like 测试未通过 / "tests failed" from matching), the engine
  re-runs the verification-class command the model itself executed during
  that turn: foreground `exec_shell` calls whose command starts with a
  known test/build prefix (`cargo test`, `npm test`, `pytest`,
  `cargo build`, … — `&&`/`;`-chained segments and leading `VAR=value`
  assignments qualify) plus `run_tests` calls (reconstructed as the
  equivalent `cargo test` invocation). The re-run is approved-replay only:
  it dispatches the exact original input through the session's own tool
  dispatcher, so the same sandbox/network policy applies and no authority
  beyond what the turn already had is granted; it never touches the
  approval gate or any capability surface. The verdict carries the
  four-element contract (verdict / dimension / evidence — command, exit
  code, originating tool_use_id, output head / failure_type), is appended
  to the transcript as a `<codesmith:runtime_event
  kind="result_verification">` user message before the next request (so
  the model must reconcile a mismatch; appended at the tail, append-only
  discipline preserved), and is surfaced as `Event::ResultVerification`
  for a status toast. A claim with no verification command behind it is
  reported as `unsubstantiated` — the hallucination signal that needs no
  re-run at all. Configure with `[verification] result_claims`
  (bool, default `true`). Unit tests in
  `engine/result_verifier.rs`; engine integration tests
  `result_verifier_replays_test_command_and_injects_verdict` /
  `result_verifier_flags_unsubstantiated_claim` /
  `result_verifier_disabled_by_config_skips_check`.
- **Three-zone prompt contract wired into the engine request path
  (#2264 Phase 2)**: the `prompt_zones` types are now load-bearing instead
  of scaffolding. `Session.messages` is an `AppendLog` — `push` is the
  only everyday mutation the type expresses (`insert` / `remove` /
  `truncate` / `clear` / index writes do not compile); sanctioned
  wholesale replacements (compaction, overflow recovery, `/edit` rollback,
  session restore, cycle reseeds, front trims) all funnel through
  `AppendLog::rebuild` with a named `RebuildReason`, recorded in an audit
  record and surfaced as `Event::TranscriptRebuilt`. The framework
  `ChatHistory` trait gains an audited `replace_all` hook (default:
  clear + push loop) that `SessionChatHistory` overrides to land mid-run
  compaction/recovery replacements in the same record atomically. The
  per-step `MessageRequest` is assembled through `ThreeZoneRequest`
  (`messages` = log slice + scratch tail only) — byte-identical to the
  legacy direct construction, pinned by the
  `three_zone_assembly_sends_verbatim_log_snapshot` regression test.
  `PrefixStabilityManager` now consumes the same `FrozenPrefix` the
  request path freezes per step, unifying the two parallel fingerprint
  implementations; tool identity hashes the full sorted JSON of every
  tool definition instead of names only, so a tool description or schema
  edit is now detected as prefix drift. `TurnScratch` is wired as the
  engine's per-turn staging area (working-set paths + the composed user
  message, committed to the log before the request loop, cleared at the
  turn boundary; the request-time scratch is empty in production).
  `/cache zones` reports live zone state including the bounded rebuild
  audit ("why did my cache reset") instead of confessing it is not wired.
- **Parse-gated file editing (P0-1)**: `write_file`, `edit_file`,
  `apply_patch`, and `fim_edit` now syntax-check writes to `.rs` / `.toml` /
  `.json` files *before* anything touches disk. The gate is regression-only —
  a write is rejected only when the file parsed cleanly before the edit and
  the new content does not, so already-broken files (and new files) stay
  editable. Rejections report the offending line and column and leave the
  file unmodified; `Cargo.lock` is exempt. Rust files that were
  rustfmt-clean before the edit are re-normalized with `rustfmt` after the
  gate passes (disclosed in the tool result) to keep subsequent patch
  anchors stable — a missing rustfmt never blocks a write. Rust parsing uses
  `syn` behind the new `parse-gate` cargo feature of `codesmith-agent-runtime`
  (enabled by `codesmith-tui` and `codesmith-tool-impls`; builds without it
  compile a pass-through gate). Configure with `[edit] parse_gate`
  (bool, default `true`).
- **Stream termination proof (P0-3)**: the rig adapter's stream mapper no
  longer forges `MessageDelta`/`MessageStop` when the provider's SSE
  connection dies silently. rig's `Final` payload is treated as the
  termination proof (rig-core 0.39 collapses the wire `finish_reason` /
  `[DONE]` evidence into it); a stream that ends without one is surfaced as
  a retryable "interrupted connection" error, so zero-content rounds
  transparently re-send and partial rounds surface the content they
  received. The engine adds a matching defense at its own seam: any stream
  that ends without `MessageStop` is treated as interrupted, never as a
  clean completion. The terminal `stop_reason` is derived instead of
  hard-coded `end_turn`: billed output reaching the requested `max_tokens`
  cap reports `max_tokens` (arming the P0-2 truncation gate on real
  traffic), tool rounds report `tool_use`. Transparent stream retries now
  also drive the structured retry banner (`retry_status`: attempt,
  countdown, reason — rendered in the TUI footer). All four rig-backed
  provider factories inject a reqwest backend that falls back to HTTP/1.1
  after the first HTTP/2 protocol failure (sticky, one replay).
- **Reasoning-effort tier in the status line**: the REPL line now carries the
  thinking tier after the model label — `🧠off` / `🧠low` / `🧠med` /
  `🧠high` / `🧠max` / `🧠auto`, mirroring the `reasoning_effort` setting that
  `/config reasoning_effort` writes and every turn sends. Under `auto` routing
  the segment reads `🧠auto` rather than tracking the per-turn router pick;
  `/status` still prints the resolved tier.

### Changed

- **The approval prompt now says what it is asking, and what each answer
  covers**: the header used to print the tool's *static description* as the
  action label — a paragraph for shell-like tools, which buried the question —
  and the legend left the scope of `[a]` to guesswork. The header is now the
  tool plus its argument summary (`⚠ approval needed: write_file — path:
  notes.txt`), and the legend states the scope the engine actually applies via
  the shared `grouping_key_kind` classifier: `[a] always allow this command
  family for this session` for shell tools, `patches to the same paths` for
  patches, `requests to the same host` for fetches, and — for every other
  tool, where the grouping key is an input hash — `this exact call`, which is
  no broader than what `[e]` denies. `[y]`/`[n]`/`[a]`/`[e]` keep their keys
  and meanings.

- **REPL status line: `🚥` mode / `🧭` context segments, a fuller in-turn
  live line, and the server-reported model name**. The permission-mode
  segment is now `🚥agent` (was `mode: agent`; the `: ` separator was dropped
  for a tighter line) and the context segment
  `🧭 42% (72k/171k)` (was `ctx …`). The transient in-turn live line now
  carries the same mode and sub-agent count segments (the count always
  shows, `🤖0` included) alongside the context percent, instead of
  ctx-percent only. The model label now prefers the model name the
  provider reports in each stream's `message_start` frame — so an
  alias gateway (e.g. BigModel's Anthropic-compatible endpoint serving a
  GLM model under a `claude-*` alias) displays the model that actually
  served the turn; the configured id shows until the first response (and
  on the startup banner), and a `/model` switch clears the stale report.
  The reported name is carried on `Event::TurnComplete.reported_model`
  and persisted in the saved session's metadata beside `model`.

- **A mistyped top-level command fails loud instead of opening the TUI**:
  `codesmith docker` and `codesmith version` used to reach the interactive TUI
  as an initial prompt, because the top-level `[PROMPT]` positional accepts
  anything. Now a single bare command-shaped word (ASCII, no spaces) with no
  `-p` and no `--continue` is read as a subcommand name and rejected with
  clap's `unrecognized subcommand '<word>'` usage error (exit code 2) plus
  hints for `-p` and `codesmith run <COMMAND>`. A word after the `--`
  terminator is never treated as a command guess — `codesmith -- docker` is a
  prompt. The `-p` hint prints the word unquoted so it pastes correctly into
  POSIX shells, PowerShell, and cmd.exe, which does not treat single quotes as
  quoting. Other prompt forms are unchanged: quoted sentences, multi-word
  tails, `-p`, and non-ASCII words still start the interactive session. See
  `docs/CLI.md` ("Prompts and subcommands").

### Fixed

- **A workspace-escape refusal now names the way out**: `read_file`, `list_dir`,
  and the other file tools refused an absolute path outside the workspace with
  nothing but `path escapes workspace` — the `ToolError::PathEscape` `Display`
  that the model-visible transcript (`engine/turn/batches.rs`) and the REPL line
  (`interactive/render.rs`) both print. A session asked to compare its checkout
  against a sibling repository therefore stalled: the model reported the refusal
  and stopped, and the user never learned the one command that unblocks it. The
  message now carries the exact `/trust add <path>` inline — the same remedy
  `docs/SUBAGENTS.md` promises for a sub-agent `cwd` — plus the
  workspace-relative alternative. The orphaned `format_tool_error` copy of that
  advice, which recommended the wider `/trust on` and had no production caller
  left after the TUI's removal, now delegates to the `Display`, so one string
  owns the remedy.
- **The REPL's live status line no longer shares a row with permanent text**:
  the transient in-turn line is drawn on stderr with no leading `\r` and no
  erase of its own, relying on an earlier `ESC[2K CR` that only happens when
  the line is marked active. Two writers then styled the same row: a frame
  drawn into an open row glued itself to streamed assistant text, and the next
  frame's `ESC[2K` erased that text with it — a pty capture of the old build
  reduced four streamed lines to `first.` before the approval prompt. A live
  frame is now a whole-row write (close any open permanent row, then `CR`
  + erase-to-end-of-line + the frame), and the approval/choice prompts
  terminate their own row when stdin is not a terminal (or the answer ended in
  EOF), where no echo newline arrives.
- **Collapsed tool lines pair each completion with its own arguments**: the
  pending tool call was a single `(name, args)` slot matched by name, so one
  message carrying two calls of the same tool — two `agent_open` calls, one per
  repository — printed the *last* call's arguments on the *first* completion
  line (and vice versa). Pending calls are now keyed by tool-call id.
- **`explore` is read-only in fact, not only in prose**: the type is documented
  as read-only (`agent-runtime::subagent`, `docs/SUBAGENTS.md`) and the prompts
  point at it for parallel sweeps, but it received the full Agent tool surface
  unless a caller narrowed it. The registry now intersects the type's tools
  with the read-only names of its own registry, so `write_file`, `edit_file`,
  and `exec_shell` are neither visible nor callable; asking for a write tool in
  `allowed_tools` fails the spawn with the refused names instead of silently
  dropping them. The narrowing also drops the spawn family — an explorer is a
  leaf.
- **Approving an out-of-workspace path now runs the call that asked**: a
  read-only tool (`read_file`, `list_dir`, `grep_files`) pointed outside the
  workspace was speculatively started while the model was still streaming — the
  early-tool-start path gates on static capabilities only — so it had already
  failed with `path escapes workspace` before the `⚠ path outside the
  workspace` question was shown. Answering `[y]`/`[a]` granted the path and
  persisted it, but the serial batch then reused that pre-approval failure
  instead of re-running the call, and the transcript reported the refusal the
  user had just allowed. Speculative dispatch now reads the same approval
  predicate as the execution planner (`turn::approval::input_requires_approval`,
  the single definition both sites share), so a gated call — an escaped path, or
  a dispatcher answering `Required`/`Suggest` for this input — never runs before
  the answer, and a gated call never reuses a speculative result. The grant
  itself was never wrong: `workspace-trust.json` and the in-process live list
  were being written correctly all along.

## [0.9.0]

### Removed

- **The ratatui TUI and the `codesmith-tui` companion binary.** CodeSmith is
  now a single `codesmith` executable: a Claude Code / Codex style line-based
  REPL is the interactive frontend, and every headless subcommand
  (exec/doctor/serve/mcp/…) runs in-process. The status line above the prompt
  shows the session facts (running state, preset tier, context-window
  occupancy, mode, running sub-agents, model, version), sessions auto-save at
  each turn boundary, and the slash-command surface carries over (view-bound
  commands degrade to explicit notes). Release channels — npm, brew, Docker,
  nix, GitHub assets, self-update — ship the one binary; installs updating
  from a two-binary release need a one-time reinstall.
- The terminal-chrome flags `--no-alt-screen`, `--mouse-capture`, and
  `--no-mouse-capture` (alt screen, mouse capture, and bracketed-paste are
  gone with the TUI), the `[tui]` and `[notifications]` config tables, and
  the `~/.codesmith/tui.toml` preferences file. Turn notifications remain
  available through the model-callable `notify` tool. Unknown keys in old
  config files are ignored on load.
- The `KEYBINDINGS` and `ACCESSIBILITY` guide pairs under `docs/` — the
  line-based REPL uses the terminal's native editing keys and scrollback,
  so there is no custom keymap or screen-reader interaction surface to
  document.

## [0.1.0] - 2026-08-25

### Added

- Initial public release of **CodeSmith**, a terminal coding agent for
  open-source and open-weight coding models (DeepSeek, Moonshot, OpenAI,
  Anthropic, NVIDIA NIM, OpenRouter, SiliconFlow, Fireworks, Novita, vLLM,
  SGLang, Ollama, and any OpenAI-compatible gateway).
- Matched binary pair: `codesmith` dispatcher CLI + `codesmith-tui` runtime.
- Streaming chat with reasoning-block rendering and per-turn thinking levels.
- Auto mode that selects model and thinking effort per turn.
- Approval-gated workspace editing, sandboxed shell execution, and YOLO mode.
- MCP client support, persistent task/session state, sub-agent teams, and a
  local skill registry.
- npm installer package, Homebrew tap, Docker image, and Nix packaging.

[Unreleased]: https://github.com/camilesing/CodeSmith/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/camilesing/CodeSmith/releases/tag/v0.1.0
