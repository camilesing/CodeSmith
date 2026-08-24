# Issue Ledger

This file is the single consolidated record of every issue reference found
in the codebase and its documentation (compiled 2026-08-24).

**Inclusion policy:**

- References to **this repository's GitHub issues** (`#N` numbers and
  `github.com/Hmbown/CodeSmith/issues` links) are treated as non-existent
  and are deliberately **not** cataloged. Where a passage described a real
  problem but carried such a number, the description is recorded here with
  the number stripped.
- Everything else is cataloged: the internal `CX#N` numbering, security
  reports (HackerOne, GHSA advisories), cross-repo `Whalescale#N`
  references, and un-numbered known issues / TODOs / limitations.
- `.zcode/plans/` (local tooling session artifacts) is out of scope, as
  are the untracked working artifacts under `docs/superpowers/plans/`.
- `crates/tui/CHANGELOG.md` is a line-for-line copy of the root
  `CHANGELOG.md`; all citations below use the root file.

Going forward, record new issue references here instead of pointing at the
retired GitHub tracker.

## 1. Internal `CX#N` references

Four internal design-issue numbers appear in TUI comments. No `CX#1`–`CX#4`
references exist in the tree.

### CX#5 — streaming gate for fenced code blocks split across deltas

A fenced code block whose opener arrives split across stream deltas must
never expose a partial fence (e.g. `foo```rust` without the closing
fence) to the renderer. The streaming line buffer gates emission until the
opener is provably complete.

- `crates/tui/src/tui/streaming/line_buffer.rs:169` (acceptance scenario)

### CX#6 — width-independent parse vs width-dependent render

The previous renderer was a single-pass `render_markdown(content, width)`
that re-parsed the source on every terminal resize. The fix splits `parse`
(width-independent block AST, cached per transcript cell) from
`render_parsed` (width-dependent wrap + span styling), making resize a
re-flow instead of a re-parse + re-flow. The perf invariant is pinned by
tests.

- `crates/tui/src/tui/markdown_render.rs:3` (module doc)
- `crates/tui/src/tui/markdown_render.rs:1378` (perf invariant)

### CX#7 — parallel tool calls render as one aggregated cell

Tool calls that run in parallel within a turn must mutate a single active
cell in place and render as ONE aggregated block (tool_routing), not one
cell per invocation. Tests lock the contract for the common parallel case.

- `crates/tui/src/tui/tool_routing.rs:63`
- `crates/tui/src/tui/ui/tests.rs:4743`
- `crates/tui/src/tui/ui/tests.rs:5061`
- `crates/tui/src/tui/ui/tests.rs:5151`

### CX#8 — live view compact vs transcript view full

Contract: the live (bottom) view keeps reasoning compact and caps tool
output; the transcript (scrollback) view shows the full body. Locked by
tests.

- `crates/tui/src/tui/history.rs:4571`

## 2. Security reports

### HackerOne report #3086545 — invisible Unicode prompt injection

Unicode Tag characters (block U+E0000–U+E007F, notably U+E0001 LANGUAGE
TAG) and zero-width characters (e.g. U+200B) are invisible to users but
are processed by the model — a hidden prompt-injection vector demonstrated
in the report. Defense: `partially_sanitize_unicode` /
`recursively_sanitize_unicode` NFKC-normalize and strip dangerous Unicode
(zero-width, bidi formatting, BOM, private-use, Tag block) from MCP
tool-call inputs, tool results, model context, and the transcript.
Design reference:
<https://embracethered.com/blog/posts/2024/hiding-and-finding-text-with-unicode-tags/>

- `crates/agent-runtime/src/sanitization.rs:6`
- `crates/agent-runtime/src/sanitization.rs:143` (U+E0001 vector)
- `crates/agent-runtime/src/engine/context.rs:308`
- `crates/agent-runtime/src/engine/host_executor.rs:373`
- `crates/agent-runtime/src/engine/host_executor.rs:1569`
- `crates/agent-runtime/src/engine/host_executor.rs:8204`
- `crates/agent-runtime/src/mcp.rs:3010`
- `docs/rfcs/extra-findings-01-unicode-sanitization.md:12`
- `CHANGELOG.md:64` (Unreleased sanitization entry)
- `ROADMAP.md:1580` (read-file observe path)

### GHSA-72w5-pf8h-xfp4 — sub-agent default privileges must be opt-in

Tightened the default privileges of sub-agents created through
`task_create` (patched in v0.8.26). Regression guards pin the contract:
`allow_shell` / shell access must be opted into explicitly, and omitted
optional fields must not silently enable shell.

- `crates/tui/src/config.rs:2526`
- `crates/tui/src/config.rs:5023` (regression test)
- `crates/tui/src/task_manager.rs:582`
- `crates/tui/src/task_manager.rs:1615` (regression test)
- `CHANGELOG.md:2690`

### GHSA-88gh-2526-gfrr — `fetch_url` network-target validation

Hardened the `fetch_url` tool's network-target validation (patched in
v0.8.26). Regression coverage pins parsing of bracketed IPv6 literals.

- `crates/tool-impls/src/tools/fetch_url.rs:338`
- `crates/tool-impls/src/tools/fetch_url.rs:687` (regression test)
- `CHANGELOG.md:2688`

### Dependency-side advisories (for completeness)

Third-party upgrades recorded in the changelog, not defects of this
codebase:

- `CHANGELOG.md:1985` — `next` 15.5.16 → 15.5.18 (GHSA-26hh-7cqf-hhc6,
  App Router middleware/proxy bypass via segment-prefetch routes) and a
  `mermaid` GHSA-family bump.

## 3. Cross-repo `Whalescale#N` references

The Whalescale desktop project tracks its integration needs in its own
issue numbers; the runtime API and TUI cite them at the implementation
sites. (Where a Whalescale number appeared paired with a CodeSmith
tracker number, only the Whalescale number is kept, per policy.)

### Whalescale#420 — graceful MCP shutdown

`StdioTransport::shutdown` sends SIGTERM and gives stdio servers a brief
grace window before tokio's `kill_on_drop` fires SIGKILL; a drop fallback
covers pools that were never shut down explicitly and transports that
have no child process.

- `crates/agent-runtime/src/mcp.rs:677`
- `crates/agent-runtime/src/mcp.rs:818` (cited as `#420`)
- `crates/agent-runtime/src/mcp.rs:3149`
- `crates/agent-runtime/src/mcp.rs:5008` (cited as `#420`)
- `crates/agent-runtime/src/engine/mod.rs:911` (cited as `#420`)

### Whalescale#439 — toast-stack queue consistency

When multiple status toasts are queued, surface the older ones as a 1–2
line strip above the footer so a burst of events is not collapsed to a
single visible message (`TOAST_STACK_MAX_VISIBLE = 3`; the footer line
stays the most-recent).

- `crates/tui/src/tui/app.rs:3097`
- `crates/tui/src/tui/ui.rs:6540` (cited as `#439`)
- `crates/tui/src/tui/ui.rs:7779` (cited as `#439`)

### whalescale#255 — runtime API CORS allow-list

The runtime API gains configurable CORS origins for the Whalescale
desktop bridge: `[runtime_api] cors_origins` in `config.toml` plus the
`CODESMITH_CORS_ORIGINS` env var, extending the built-in dev origins
while preserving first-seen order.

- `crates/tui/src/runtime_api.rs:78`
- `crates/tui/src/runtime_api.rs:2044`
- `crates/tui/src/runtime_api.rs:3684`
- `crates/tui/src/config.rs:1187`
- `crates/tui/src/config.rs:1218`
- `crates/tui/src/main.rs:668`
- `crates/tui/src/main.rs:1699`
- `CHANGELOG.md:3766` (runtime API quartet entry)

### whalescale#260 — `archived_only` thread filtering

Thread listing accepts `archived` / `status` / `archived_only` query
params so the desktop UI can request archived-only views.

- `crates/tui/src/runtime_api.rs:216`
- `crates/tui/src/runtime_api.rs:226`
- `crates/tui/src/runtime_api.rs:3857`
- `crates/tui/src/runtime_threads.rs:553`

### whalescale#256 — `PATCH /v1/threads/{id}`

The runtime API accepts a PATCH endpoint so the UI can flip persistent
thread state (title, archived flag) without a full rewrite, with a
`schema_version` bump.

- `crates/tui/src/runtime_api.rs:3770`
- `crates/tui/src/runtime_threads.rs:584`

### whalescale#261 — `GET /v1/usage` aggregation

Usage endpoint aggregates per-turn token and cost totals by day/model for
the desktop UI.

- `crates/tui/src/runtime_api.rs:3968`
- `crates/tui/src/runtime_threads.rs:629`
- `crates/tui/src/runtime_threads.rs:993`

## 4. Un-numbered known issues and TODOs in source

### TODO — route sub-agent permission requests to the approval dialog

The only `TODO` in the workspace. A sub-agent permission request
currently emits just a status message ("X needs permission for Y");
routing it to the real approval dialog waits on UI support.

- `crates/agent-runtime/src/engine/team_inbox.rs:82`

### OSC 8 rendering corruption (issue forthcoming)

A Windows session reported stray bytes eating the leading column of the
next line and duplicating the composer panel during scroll (screenshot
showed `"eepseek-v4-flash"` with the leading `d` consumed and three
overlapping composer panels). v0.8.8 also surfaced macOS corruption
(`"526sOPEN"` instead of `"526   OPEN"`): OSC 8 wrappers emitted inside
ratatui `Span` content are handled byte-wise — the grapheme filter drops
the bare ESC byte but paints every other wrapper byte into a buffer cell,
drifting columns. Mitigation: OSC 8 links are default-off on every
platform until emitted out-of-band of the buffer pipeline; opt back in
via `[ui] osc8_links = true`. No tracking issue was ever created.

- `crates/tui/src/tui/ui.rs:287`–`303`

### Word-wrap overflow for overlong tokens (fixed, pinned by tests)

The paragraph wrap (`render_line_with_links`) and code-block wrap
(`wrap_text`) were word-based: a single word wider than the available
width was placed alone on a line and silently overflowed the right edge —
long URLs, paths, hashes, and no-whitespace CJK runs all hit this. The
fix hard-breaks overlong words; the regression suite pins it at widths 40
and 80.

- `crates/tui/src/tui/markdown_render.rs:1761` (bug description)
- `crates/tui/src/tui/markdown_render.rs:1866` (code-block wrap pin)

### Paste must never auto-submit (QA guard)

Auto-submit would replace the composer with a "working / thinking"
status chip and clear the composer text; either signal in the PTY dump
means the bug fired.

- `crates/tui/tests/qa_pty.rs:241`

### Skill discovery ignored vendor-nested subdirectories (fixed, pinned)

Users organizing skills under vendor/category subdirectories (cloned
skill repos bundling several skills) were silently dropped by the old
single-level `read_dir`, which only ever surfaced
`<root>/<skill>/SKILL.md` and ignored `<root>/<vendor>/<skill>/SKILL.md`.

- `crates/agent-runtime/src/skills/mod.rs:1524` (regression test)

### Long-token row overflow in the pending-input preview

A token longer than the wrap budget is flushed as its own overflowing row
— deliberately, to avoid a long URL fanning out into N junk-ellipsis
rows (the known codex-TUI behavior this avoids).

- `crates/tui/src/tui/widgets/pending_input_preview.rs:278`

## 5. Known issues, limitations, and deferred work in documents

GitHub tracker numbers from the original passages are stripped per
policy; descriptions and version context are kept.

### CHANGELOG "Known issues" sections

- **v0.8.32** (`CHANGELOG.md:1869`) — terminal-native text selection can
  still be blocked while the agent is thinking or streaming. v0.8.32
  removed the noisy Shift-to-bypass-mouse-capture path (the "scroll
  demon"), but the replacement selection path was not complete yet; the
  text-selection fix was planned for v0.8.33.
- **v0.8.25** (`CHANGELOG.md:2862`) — Windows 10 conhost flicker
  regression: the viewport-reset escape sequence added in v0.8.22 needs a
  Windows guard (deferred to v0.8.26). Snapshot system still snapshots
  every turn regardless of workspace changes (write-aware skip planned
  for v0.8.26). `▏` glyph leak in code blocks, mouse selection crossing
  the sidebar, drag-select edge auto-scroll, and mid-run MCP server
  stderr capture — all deferred to v0.8.26. Later entries show the
  drag-select auto-scroll, glyph, and MCP stderr fixes shipped in v0.8.26
  (`CHANGELOG.md:2736`–`2753`), and cross-terminal flicker fixes in the
  v0.8.27–v0.8.29 range (`CHANGELOG.md:2457`, `:2525`).
- **v0.8.24** (`CHANGELOG.md:2957`) — Windows flicker/shake root cause:
  the viewport-reset sequence (`\x1b[r\x1b[?6l\x1b[H\x1b[2J\x1b[3J`) may
  trigger a full screen clear on every repaint under conhost; a platform
  guard or less aggressive sequence was needed.
- **v0.8.23** (`CHANGELOG.md:3039`) — mid-run MCP server stderr is
  suppressed: a stdio server that spawns successfully but crashes later
  (e.g. during `initialize`) had no stderr capture; planned for v0.8.24,
  shipped in v0.8.26 (`CHANGELOG.md:2744`).

### docs/INDEX.md — code-index v1 limitations

- `docs/INDEX.md:94` — references are name-based (lexical), not
  scope-resolved; the index is bound to the workspace root (worktree
  files not re-indexed in v1); background runtime threads run without the
  index; semantic search (`[index.semantic]`) is a reserved seam with no
  compiled backend.

### docs/SANDBOX.md — what the sandbox does NOT protect against

- `docs/SANDBOX.md:268` — network attacks (Linux and Windows v1 leave
  network open), git hook / fsmonitor execution, memory attacks, timing
  side channels, resource exhaustion (no CPU, fd, or disk-I/O limits),
  kernel vulnerabilities, and supply chain. Platform-specific gaps:
  Linux seccomp whitelist may need updates for new syscalls; macOS
  Seatbelt profiles generated at runtime could be misconfigured to be
  too permissive.

### docs/KEYBINDINGS.md — configurable keymap deferred

- `docs/KEYBINDINGS.md:129` — configurable keymap and `tui.toml` remain
  deferred: the `TuiPrefs` struct and loader exist in `settings.rs` but
  are not wired at startup; the named-binding registry that would let
  `~/.codesmith/tui.toml` override individual entries is still pending.
  (Chinese mirror: `docs/KEYBINDINGS_cn.md:129`.)

### docs/EXTENSIONS.md — disable takes effect on reload

- `docs/EXTENSIONS.md:78` — `/extension disable <id>` marks the extension
  disabled, but the effect lands on the next `/extension reload` (same
  reload caveat).

### docs/superpowers/todo.md — §F extension-system handoff

- `docs/superpowers/todo.md` — §F5 (dylib loading) and §F2 (events,
  handler chains, live reload) are complete. Remaining phases are
  on-demand (no spec/plan yet): **§F3** EventBus real impl
  (`crates/extensions/src/bus.rs` `subscribe`/`publish` currently return
  `ExtensionError::Unimplemented`), **§F4** registerProvider, **§F6**
  Renderers, **§F7** Shortcut + Flag, **§F8** Embedding API. Hot-load is
  permanently out (spec §2.4 "never"). Flaky-test baselines recorded
  there: `streamable_http` (agent-runtime) and `runtime_api` (tui) —
  pre-existing, isolate-rerun if they fire.

### docs/plans/codebase-health.md — cleanup backlog

- `docs/plans/codebase-health.md:34` — drive `allow(dead_code)` in
  `crates/agent-runtime/src/engine/` to zero (or attach a migration-issue
  link to each survivor); retire comments pointing at deleted code;
  merge/delete the TUI mirror modules (`tui/src/compaction/`,
  `tui/src/prompts.rs`, `tui/src/mcp.rs`, `tui/src/sandbox/`,
  `tui/src/execpolicy/`) once confirmed pure re-exports.
- `docs/plans/codebase-health.md:37` — about 12 sub-agent tools deprecated
  since v0.8.33 (`agent_spawn`, `agent_result`, `agent_wait`,
  `delegate_to_agent`, …) are still registered in the catalog, costing
  tool-surface and prompt budget.

### ROADMAP.md — known gaps and deferred re-wires

- **Thinking-only handling, by-design gaps** (`ROADMAP.md:1922`–`1940`) —
  goal-continuation and inline-REPL resume branches are deferred (infra
  still live but unwired: `tool_state/goal.rs`, `repl/`); the
  placeholder `"(reasoning omitted)"` Thinking block for tool-call turns
  is not injected by the executor (DeepSeek thinking-mode requires
  `reasoning_content` on tool-call assistant messages). The seam-3
  parallel-dispatch gap named there as the last "still to come" item has
  since closed (slice 40; `crates/agent-runtime/src/engine/host_executor.rs:251`).
- **Compaction closure** (`ROADMAP.md:1536`–`1565`) — 25a
  (summary-prompt merge) and 25b (attachment reinject) landed; **25c**
  `post_compact_cleanup` is still deferred (merge-XOR-cleanup mutual
  exclusion plus divorced `CompactionProbe` slots); the read-file observe
  site has no production caller yet and is an independent follow-up
  slice.
- **Kept superseded members under `#[allow(dead_code)]`**
  (`ROADMAP.md:1716`–`1722`) — `layered_context_checkpoint` (zero
  callers; kept for nav-aids re-wire reference), `Engine::recover_context_overflow`
  (capacity-cascade reference), the KoD cluster (Knowledge-on-Demand,
  planned), `rx_user_input` (paired lifetime with the tui sender),
  `tool_exec_lock` (couples to the deferred Gate-A CapacityController),
  `EarlyToolResult` / `EarlyToolTask` (speculative dispatch), and
  reserved `CancelReason` enum variants.

### docs/rfcs/2189-persistence-sqlite.md — persistence pain points

- `docs/rfcs/2189-persistence-sqlite.md:68` — the five pain points
  motivating the SQLite persistence RFC: listing threads/sessions/tasks
  requires scanning and deserializing every file; filtering requires full
  scans; no transactional consistency (a crash between saving a turn and
  its items can leave orphans); JSONL event replay is O(n) with no
  indexing; six different schema-version constants across four modules.
