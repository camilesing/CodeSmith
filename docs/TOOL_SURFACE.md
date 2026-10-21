# Tool surface

Why these specific tools, in this groupings, and how each one is meant to be
chosen over the available shell equivalent. Companion to `crates/agent-runtime/src/prompts/agent.txt`.

## Design stance

- **Dedicated tools over `exec_shell` whenever the dedicated tool returns
  structured output.** Bash escaping is error-prone and platform behavior
  varies (GNU vs BSD `grep`, `rg` is not always installed). Structured
  output also frees the model from re-parsing free-form text.
- **`exec_shell` for everything else.** Build, test, format, lint, ad-hoc
  commands, anything platform-specific. We don't try to wrap the long tail.
- **Drop tools that don't beat their shell equivalent.** Two-tool aliases
  for the same backing operation are a model trap — the LLM will alternate
  between them and the cache hit rate suffers.

## Current surface

### File operations

| Tool | Niche |
|---|---|
| `read_file` | Read a UTF-8 file. PDFs auto-extracted via the bundled pure-Rust `pdf-extract` reader by default (no Poppler install required); opt into the external `pdftotext -layout` route for column-heavy PDFs with `prefer_external_pdftotext = true` in `~/.config/deepseek/settings.toml`. `pages: "1-5"` slices large docs. |
| `list_dir` | Structured, gitignore-aware listing. Preferred over `exec_shell("ls")`. |
| `write_file` | Create or overwrite a file. `.rs`/`.toml`/`.json` writes are parse-checked first: breaking a file that parsed cleanly before is rejected with the error location. |
| `edit_file` | Search-and-replace inside a single file. Cheaper than a full rewrite. Same pre-write parse gate as `write_file`; rustfmt-clean Rust files are re-normalized after the edit. |
| `apply_patch` | Apply a unified diff. The right tool for multi-hunk edits. The whole batch is parse-gated before any file is touched (all-or-nothing). |
| `fim_edit` | Edit a file using Fill-in-the-Middle completion: `prefix_anchor` and `suffix_anchor` frame the region to replace, DeepSeek's FIM endpoint generates the new content, and the same pre-write parse gate as `edit_file` applies. |
| `retrieve_tool_result` | Read summaries or slices of prior large tool outputs spilled to `~/.codesmith/tool_outputs/`; use `summary`, `head`, `tail`, `lines`, or `query` instead of replaying the whole result. |
| `handle_read` | Read bounded projections from `var_handle` payloads held by live tool environments. This is the foundation for RLM sessions, sub-agent transcripts, and other large symbolic payloads. |

### Search

| Tool | Niche |
|---|---|
| `grep_files` | Regex search file contents within the workspace; structured matches + context lines. Pure-Rust (`regex` crate), no `rg`/`grep` shell-out. |
| `file_search` | Fuzzy-match filenames (not contents). Use when you know roughly the name. |
| `symbol_search` | Search the persistent workspace symbol index (functions, structs, traits, classes, …) by name substring. Indexed, so faster and more precise than `grep_files` for "where is X defined?". Registered only when the session enabled the code index. |
| `find_references` | Definition sites plus every lexical occurrence of an exact symbol name, via the same persistent code index. Faster than repeated `grep_files` on large repos. |
| `lsp_navigate` | Type-aware definition / references / hover at a 1-based position via the session's LSP server — resolves the symbol at a call site (trait methods, re-exports) more precisely than the name-based `symbol_search`. Fails closed when LSP is disabled or no server exists for the language. |
| `web_search` | DuckDuckGo by default with Bing fallback; Bing, Tavily, Bocha, Metaso, and Baidu are selectable in config. Ranked snippets + `ref_id` for citation. |
| `fetch_url` | Direct HTTP GET on a known URL. Faster than `web_search` when the link is already known. HTML stripped to text by default. |
| `web.run` | Browse the web (search/open/click/find/screenshot/image_query) and return structured results with `ref_id`s for citations. |
| `finance` | Live market quote for a stock, ETF, or crypto ticker via Yahoo Finance-style public endpoints. |

### Shell

| Tool | Niche |
|---|---|
| `exec_shell` | Run a shell command. Foreground runs are cancellable, but use them only for bounded commands; timeout kills the process and returns a background-rerun hint. |
| `exec_shell_wait` | Poll a background task for incremental output. Canceling the turn stops waiting without killing the task. |
| `exec_shell_interact` | Send stdin to a running background task and read incremental output. |
| `exec_shell_cancel` | Cancel one running background shell task by id, or all running background shell tasks when explicitly requested. |
| `task_shell_start` | Start a long-running command in the background and return immediately. Preferred over foreground shell for diagnostics, tests, searches, and servers that may run for minutes. |
| `task_shell_wait` | Poll a background command. If `gate` is supplied after completion, record structured gate evidence on the active durable task. |
| `exec_wait` / `exec_interact` | Legacy aliases of `exec_shell_wait` / `exec_shell_interact`, kept registered for compatibility with older prompts. |

When a foreground shell command times out, the process is not continued
silently. The tool result tells the model to rerun long work with
`task_shell_start` or `exec_shell` with `background = true`, then poll with
`task_shell_wait` or `exec_shell_wait`.

Interactive shell jobs are also visible through `/jobs`: a plain list of
running shell jobs from the same shell manager as
`exec_shell`/`task_shell_start`, printing each job's id, elapsed time, and
command. It has no subcommands — inspect, stdin, and cancel go through the
shell tools (`exec_shell_wait`, `exec_shell_interact`, `exec_shell_cancel`).

Shell permission policy is evaluated by `crates/execpolicy`. Deny prefixes are
checked before trusted prefixes and block matching commands regardless of layer.
Trusted prefixes only skip approval in modes that permit trust shortcuts. Typed
ask records are currently a narrow foundation: when one matches under
`AskForApproval::Never`, the command is rejected because the runtime cannot ask
the user; existing allow/deny behavior is otherwise unchanged.

### MCP manager discovery

MCP server configuration is surfaced in the REPL through `/mcp` and the
`mcp_config_path` row in `/config`. `/mcp` shows the resolved config path,
server enabled/disabled state, transport, command or URL, timeouts, connection
errors, and discovered tools/resources/prompts. It supports narrow manager
actions for init, add, enable, disable, remove, validate, and reload/reconnect.
Config edits are written immediately, but the model-visible MCP tool pool is
restart-required after edits. Discovered tools use the runtime names shown to
the model, such as `mcp__<server>__<tool>`.

### Git / diagnostics / testing

| Tool | Niche |
|---|---|
| `git_status` | Inspect repo status without running shell. |
| `git_diff` | Inspect working-tree or staged diffs. |
| `git_log` | Run `git log` with optional path and author/date filters. |
| `git_show` | `git show` for a specific revision with optional patch and stats. |
| `git_blame` | `git blame` on a file with optional revision and line-range controls. |
| `diagnostics` | Workspace, git, sandbox, and toolchain info in one call. |
| `run_tests` | `cargo test` with optional args. |

### Task management and durable work

| Tool | Niche |
|---|---|
| `update_plan` | Optional high-level strategy metadata for complex multi-phase work; keep `checklist_write` as the primary progress surface. |
| `task_create` | Create/enqueue a durable background task through `TaskManager`. This is the real executable work object for long-running agent work. |
| `task_list` | List durable tasks with status and linked runtime ids. |
| `task_read` | Read durable task detail: thread/turn linkage, timeline, checklist, gates, artifacts, PR attempts, GitHub events. |
| `task_cancel` | Cancel a queued or running durable task. Approval-required. |
| `task_stop` | Unified stop for background tasks, workers, teammates, shell jobs, and durable tasks — accepts unified background task ids, agent ids/session names, teammate names, shell task ids, and durable task ids. Approval-required. |
| `checklist_write` | Granular progress under the active thread/task. Checklist state is subordinate to the durable task. |
| `checklist_add` / `checklist_update` / `checklist_list` | Single-item checklist operations. |
| `todo_write` / `todo_add` / `todo_update` / `todo_list` | Compatibility aliases for the checklist tools. Existing sessions keep working, but new prompts should use `checklist_*`. |
| `note` | One-off important fact for later. |

### Session runtime state

| Tool | Niche |
|---|---|
| `task_create_v2` / `task_update_v2` / `task_get_v2` / `task_list_v2` | Structured Task V2 records with subject, description, dependencies, and status (`pending` → `in_progress` → `completed`); persist across turns. |
| `create_goal` / `get_goal` / `update_goal` | Runtime goal lifecycle: create or replace the persistent goal, inspect its state (objective, status, token budget, evidence, blocker), and update it — `update_goal` is the LLM-as-judge completion gate. |
| `enter_plan_mode` / `exit_plan_mode` / `write_plan_file` | Plan-mode lifecycle: enter investigation/design mode (read-only), write the plan file (the only writable file in plan mode), and exit with user approval. |
| `enter_worktree` / `exit_worktree` | Create an isolated git worktree and switch the session into it; exit restores the original directory (keep or remove the worktree). Approval-required. |

### Verification gates and artifacts

| Tool | Niche |
|---|---|
| `task_gate_run` | Run an approved verification command and attach structured evidence to the active durable task: command, cwd, exit code, duration, classification, summary, and log artifact. |

Large logs and command outputs should be artifacts with compact summaries in the transcript. `task_gate_run` handles this automatically for active durable tasks.

### GitHub context and guarded writes

| Tool | Niche |
|---|---|
| `github_issue_context` | Read-only issue context via `gh issue view`; large bodies become task artifacts when possible. |
| `github_pr_context` | Read-only PR context via `gh pr view`; optional diff capture via `gh pr diff --patch`; large bodies/diffs become task artifacts when possible. |
| `github_comment` | Approval-required issue/PR comment with structured evidence. |
| `github_close_issue` | Approval-required issue closure. Requires non-empty acceptance criteria and evidence; refuses dirty worktrees unless explicitly allowed. Never use for PRs. |
| `github_close_pr` | Approval-required PR closure. Requires the same structured evidence as issue closure and keeps PR wording in tool output/audit records. |

### PR attempts

| Tool | Niche |
|---|---|
| `pr_attempt_record` | Capture the current git diff as attempt metadata plus a patch artifact on a durable task. |
| `pr_attempt_list` | List attempts recorded on a task. |
| `pr_attempt_read` | Inspect one recorded attempt and its artifact reference. |
| `pr_attempt_preflight` | Run `git apply --check` against an attempt patch. No worktree mutation. |

### Automations

| Tool | Niche |
|---|---|
| `automation_create` | Create a scheduled automation. Approval-required. |
| `automation_list` / `automation_read` | Inspect durable automations and recent runs. |
| `automation_update` | Update prompt, schedule, cwds, or status. Approval-required. |
| `automation_pause` / `automation_resume` / `automation_delete` | Lifecycle controls. Approval-required. |
| `automation_run` | Run an automation now; the run enqueues a normal durable task. Approval-required. |

### Agent teams

Registered only when the `agent_teams` feature flag is enabled.

| Tool | Niche |
|---|---|
| `team_create` | Create a team for coordinating multiple agents: team config file, shared task directory, current session registered as lead. One team per leader session. |
| `team_delete` | Remove the current team and its task directories; validates no active teammates remain. |
| `send_message` | Message a teammate via file-based mailbox — plain text, broadcast (`to: "*"`), or structured protocol messages (shutdown, plan approval, permission requests). |

### Code review and recall

| Tool | Niche |
|---|---|
| `review` | Run a structured code review for a file, git diff, or GitHub pull request. |
| `recall_archive` | Search prior context cycles for content not in the current briefing. Use sparingly — frequent recalls mean the briefing was too sparse. |
| `search_sessions` | Search past session transcripts in this workspace by content (BM25), returning matching excerpts with session id and title. Lazy scan — no index; same-workspace sessions only. |
| `knowledge_recall` | Search Knowledge On Demand memory files by keyword. Registered only when KoD is enabled. |

### Documents and media

| Tool | Niche |
|---|---|
| `pandoc_convert` | Convert a document between formats via the local `pandoc` binary; registered only when `pandoc` is present. Curated format whitelist, structured errors — no shell-out. |
| `image_ocr` | Extract text from an image (PNG/JPEG/TIFF) via local OCR — macOS Vision framework, or tesseract elsewhere. Registered only when an OCR backend exists. |
| `image_analyze` | Analyze an image (PNG, JPEG, GIF, WebP, BMP) with the configured `[vision_model]`. Registered only when a vision model is configured. |

### Workspace utilities

| Tool | Niche |
|---|---|
| `project_map` | High-level project structure map: key files plus a tree view. |
| `validate_data` | Validate JSON or TOML from inline input or a workspace file. |
| `load_skill` | Load a skill's SKILL.md body plus companion file list into the next turn's context — one call instead of `read_file` + `list_dir` against the path in the system prompt's `## Skills` section. |
| `manage_mods` | List, write, and activate CodeSmith script mods (Rhai); `list` also shows the resolved policy layer (kind → effective value → winning mod id); `session_write` binds an ephemeral per-session experiment mod from inline source (identical re-writes skip re-approval). Registered only when the mod layer is live; activation and file writes are per-action approval-gated. |
| `request_user_input` | Ask the user 1–3 short questions and return their selections. |
| `revert_turn` | Roll the working tree back to the snapshot taken before a recent turn (`turn_offset` 1-based, max 50); conversation history is untouched. Approval-required. |
| `notify` | Fire a single desktop notification (OSC 9 / terminal bell) through the `[notifications]` config pipeline. Use sparingly. |

### Slop ledger

Durable tracking of unresolved architectural residue (compatibility shims, unmigrated callers, duplicate concepts, stale docs/tests, suspected dead code). The ledger JSON file is auto-created on first append.

| Tool | Niche |
|---|---|
| `slop_ledger_append` | Append one or more entries to the ledger. |
| `slop_ledger_query` | Filter entries by bucket, severity, status, or text search. |
| `slop_ledger_update` | Update an entry's status (resolved, accepted, in-progress). |
| `slop_ledger_export` | Export the ledger as a Markdown report for handoffs or GitHub issues. |

### Sub-agents

v0.8.33 began moving large tool outputs toward symbolic handles: tools return
small `var_handle` objects, and `handle_read` retrieves bounded slices, counts,
or JSON projections from the backing environment. This keeps the parent
transcript small while preserving a recovery path to the full payload.

The active model-facing sub-agent surface is persistent:

| Tool | Niche |
|---|---|
| `agent_open` | Open a named sub-agent session for independent work. Returns a session projection immediately so the parent can keep coordinating. |
| `agent_eval` | Send follow-up input, block for completion, or fetch the current projection/transcript handle for an existing session. |
| `agent_close` | Cancel or release a sub-agent session by name or id. |
| `tool_agent` | Open the experimental fast-lane execution agent (Fin): the active provider's light tier with thinking forced off, for simple tool-bound work — OCR, file/search lookups, fetches, command probes — where the parent should keep its planning context clean. |
| `agent_run` | Run a sub-agent inline and wait for the result (foreground), or spawn it in the background — the foreground form is for tight feedback loops where you need the answer now. |
| `agent_memory_read` / `agent_memory_write` / `agent_memory_edit` | Read, create/overwrite, and search-replace files inside a sub-agent's scoped Agent Memory directory (opt-in via `agent_open`'s `memory` parameter). Functional only for memory-scoped children. |

The spawn family (`agent_open`, `agent_run`, `agent_spawn`, `tool_agent`,
`agent_close`) declares itself concurrency-safe and carries no approval
requirement of its own, so several spawn calls in one assistant message open
their children at once instead of queueing behind one prompt per child. That is
not a gate being lifted: a child inherits the parent's approval posture, and a
non-auto parent's child runs `permission_mode: "ask"`, so nothing a child does
escapes the gate the parent would have faced. See the batch rules in
[ARCHITECTURE.md](ARCHITECTURE.md) ("Tool Execution") for what else may share a
concurrent batch.

See `agent.txt` for the delegation protocol and
[`SUBAGENTS.md`](SUBAGENTS.md) for the role taxonomy
(`general` / `explore` / `plan` / `review` / `implementer` /
`verifier` / `tool_agent` / `team` / `coordinator_worker` / `custom`).

`agent_open` defaults to a fresh child conversation. Pass
`fork_context: true` for continuation-style work or multi-perspective reviews
that should inherit the parent's context. In fork mode, the runtime preserves
the parent prefill/prompt prefix byte-identically where available so DeepSeek's
prefix cache can be reused, then appends the child role instructions and task.

### Recursive LM sessions

RLM is now persistent as well:

| Tool | Niche |
|---|---|
| `rlm_session_objects` | List compact cards for the active prompt, session metadata, transcript, latest user message, and per-message refs. |
| `rlm_open` | Open a named Python REPL over a file, inline content, or URL. |
| `rlm_eval` | Run bounded Python against that session, using deterministic code and in-REPL semantic helpers such as `sub_query_batch`. |
| `rlm_configure` | Adjust output feedback, child-query timeout/depth, and session-sharing settings. |
| `rlm_close` | Shut down the Python runtime and return final session stats. |

`rlm_open` also accepts `session_object`, a stable ref returned by
`rlm_session_objects`, such as `session://active/system_prompt`,
`session://active/transcript`, or `session://active/messages/0`. This loads
the selected object into the RLM REPL and returns only metadata to the parent
transcript. Transcript objects keep thinking blocks and large tool results as
compact metadata; inspect large payloads through returned `var_handle` values
and `handle_read`, not by asking the parent transcript to paste the raw text.

Large RLM outputs should come back as `var_handle`s. Use `handle_read` for
bounded text slices, line ranges, counts, or JSONPath projections instead of
replaying the full value into the parent transcript.

Inside `rlm_eval`, the loaded source is available as `_context`; `content` is
also bound as a compatibility alias because agents naturally reach for it
during Python analysis. The shorter `context` and `ctx` names
are intentionally not bound so user variables can use them without colliding
with the bootstrap.

Child calls are wrapped in the RLM bridge's per-child timeout — 120
seconds by default, overridable per context via `rlm_configure`'s
`sub_query_timeout_secs` (clamped to 1..=600). The helpers `sub_query`,
`sub_query_batch`, `sub_query_map`, and `sub_rlm` still accept a
`timeout_secs` keyword for compatibility with common agent guesses — it
remains ignored (the per-context setting is the knob).

`finalize(value, confidence=...)` preserves JSON-serializable values. Strings
become text handles; dicts, lists, numbers, booleans, and null become JSON
handles that `handle_read` can project with JSONPath.

### Session relay

`/relay [focus]` asks the current agent to write `.codesmith/handoff.md` as a
compact `# Session relay` artifact for the next thread. The filename remains
for compatibility with existing prompt loading and older sessions; the visible
mental model is relay / 接力.

Aliases: `/batonpass`.

Use it before a long break, compaction, or moving work to a fresh session. The
relay should preserve the goal, current Work checklist item, changed files,
decisions, verification state, and one concrete next action.

### Parallel fan-out: cost-class caps

Two tools offer parallel fan-out with different concurrency limits that
reflect very different cost classes:

| Tool | What each child does | Wall-clock | Token cost | Cap |
|---|---|---|---|---|
| `agent_open` | Full sub-agent loop (planning, tool calls, multi-turn streaming, can open children) | minutes | thousands of tokens | 10 in flight by default (`[subagents].max_concurrent`, hard ceiling 20) |
| `rlm_eval` helper `sub_query_batch` | One-shot non-streaming Chat Completions calls pinned to `deepseek-v4-flash` inside a live RLM session | seconds | ~hundreds of tokens | 16 per call |

The caps appear in each tool's description and error messages so the model
(and the user) can choose the right tool for the job. If one sub-agent is
enough but you need parallel semantic lookups over the same loaded context,
prefer `rlm_eval` with `sub_query_batch`; if each task needs its own
tool-carrying agent loop, use `agent_open` and wait for running sessions to
complete or cancel no-longer-needed running sessions with `agent_close`.

## Removed legacy aliases and surfaces

These one-shot sub-agent fan-out names are not part of the active
model-facing surface. Do not use them in active guidance:
`agent_wait`, `agent_result`, `agent_send_input`,
`agent_assign`, `agent_resume`, `agent_list`, `spawn_agent`,
`delegate_to_agent`, `send_input`, and `close_agent`.

`agent_spawn` is the one survivor: it is still registered in the tool
registry (`AgentSpawnTool`, `crates/cli/src/tools/registry.rs`) as the
background one-shot alias — `agent_open` with `run_in_background = true`
is equivalent — and sub-agents recurse through it (see
`docs/SUBAGENTS.md`). New guidance should still prefer
`agent_open` / `agent_eval`.

The model-facing `rlm` surface is the persistent
`rlm_open` / `rlm_eval` / `rlm_configure` / `rlm_close` session form.

Compatibility results may include a `_deprecation` block shaped
like this:

```json
{
  "_deprecation": {
    "this_tool": "spawn_agent",
    "use_instead": "agent_spawn",
    "removed_in": "0.8.0",
    "message": "Tool 'spawn_agent' is deprecated; switch to 'agent_spawn' before v0.8.0."
  }
}
```

This is a legacy/compatibility note, not the active recommended surface.

## Release smoke: verify the live names

When validating a release, verify the model-visible registry names directly.
Do not grep random handler function names; handler names are allowed to drift
while the registry contract stays stable.

Version smoke:

```bash
codesmith --version
```

Tool-surface smoke:

```bash
rg -n '"handle_read"|"rlm_open"|"rlm_eval"|"rlm_configure"|"rlm_close"|"agent_open"|"agent_eval"|"agent_close"' crates/cli/src
rg -n 'handle_read|rlm_open|rlm_eval|rlm_configure|rlm_close|agent_open|agent_eval|agent_close' docs crates/agent-runtime/src/prompts crates/cli/src/tools
```

The canonical live names are:

- `handle_read`
- `rlm_open`, `rlm_eval`, `rlm_configure`, `rlm_close`
- `agent_open`, `agent_eval`, `agent_close`

Active guidance should not advertise the one-shot names
`agent_wait`, `agent_result`, or a foreground `rlm` tool
(`agent_spawn` stays registered as the
background/compat alias described above). Compatibility code and
stored results may still mention these names.

## Why we don't ship a single `bash` tool

Single-`bash` agents (Claude Code's design) are powerful but hand the model
all the foot-guns of shell scripting: quoting, platform divergence,
side-effects from misread cwd, `cd` not persisting between calls, etc. Our
file tools are also significantly cheaper to render in the transcript
(structured JSON-shaped output collapses better than `ls -la` walls of text).

The model can always fall back to `exec_shell` when something is missing.
The dedicated tools just take the common 80% off the shell escape-hatch.
