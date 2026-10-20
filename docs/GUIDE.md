# CodeSmith User Guide

This guide is for your first hour with CodeSmith. It explains the main
workflow, the important safety controls, and where to go next when you need a
complete reference.

CodeSmith has deeper reference documents for installation, configuration,
providers, modes, tools, and operations. Use this page as a guided
walkthrough, then follow the "Next" links when you need every option.

## 1. Welcome to CodeSmith

CodeSmith is a terminal coding agent. You run it from a workspace, give it a
task, and it can use structured tools to inspect files, run commands, edit
code, and report back with evidence.

The important difference from a normal chat model is that CodeSmith is built
around a harness:

- It keeps the active workspace and session visible.
- It routes each turn through explicit modes and approval rules.
- It shows tool calls in the transcript instead of hiding the work.
- It can preserve sessions, fork conversations, and continue later.
- It can run sub-agents for focused background work.

You can use CodeSmith for small questions:

```text
Explain the authentication flow in this repository.
```

You can also use it for multi-step work:

```text
Find the failing validation path, propose a fix, and wait for my approval
before editing files.
```

For a new repository, start conservatively. Ask CodeSmith to explore and plan
before asking it to change files. That gives you a reviewable path and makes it
easier to catch wrong assumptions early.

Next: [ARCHITECTURE.md](ARCHITECTURE.md) explains the internal harness and
runtime model.

## 2. First Launch

Install CodeSmith with the path that fits your machine. Each supported install
path provides the single `codesmith` binary, which hosts both the headless
subcommands and the line-based interactive REPL.

```bash
# npm
npm install -g codesmith

# Cargo
cargo install codesmith-cli --locked

# Homebrew
# The tap/formula name differs from the package name; it installs codesmith.
brew tap camilesing/codesmith
brew install codesmith
```

Docker is also available when you want an isolated runtime:

```bash
docker volume create codesmith-home
docker run --rm -it \
  -e CODESMITH_API_KEY="$CODESMITH_API_KEY" \
  -v codesmith-home:/home/codesmith/.codesmith \
  -v "$PWD:/workspace" \
  -w /workspace \
  ghcr.io/camilesing/codesmith:latest
```

Launch CodeSmith from the repository or directory you want it to work in:

```bash
codesmith
```

On first launch there is no wizard — CodeSmith goes straight into the REPL.
Set the provider and its API key first: DeepSeek is the default provider
fallback when nothing is configured, and without a key the first turn simply
fails with a provider auth error.

The same setup paths work outside the REPL: `--provider`,
`CODESMITH_PROVIDER`, or a `provider = "<id>"` line in the config. The most
direct path for the fallback provider is:

```bash
codesmith auth set --provider deepseek
```

You can also provide a key through the environment:

```bash
export DEEPSEEK_API_KEY="your-key"
codesmith
```

CodeSmith config is stored under `~/.codesmith/config.toml`.

After setup, run a doctor check:

```bash
codesmith doctor
```

Use the JSON form when you need a machine-readable report for an issue:

```bash
codesmith doctor --json
```

If the doctor command reports that a rejected key came from the environment,
remove or replace that environment variable before testing saved config again.

Next: [INSTALL.md](INSTALL.md) covers platform-specific install paths,
[CONFIGURATION.md](CONFIGURATION.md) covers config resolution, and
[PROVIDERS.md](PROVIDERS.md) covers provider IDs and credentials.

## 3. Your First Task

Start with a read-only task in a real workspace:

```text
Map the repository structure and tell me where the CLI entrypoint lives.
```

Then ask for a focused plan:

```text
I want to add a small validation for empty config values. Inspect the relevant
code and propose the smallest safe change before editing anything.
```

When you are ready for edits, be specific about the acceptance criteria:

```text
Implement the validation you proposed. Keep the change scoped to config
parsing, add or update the narrowest test, and run the relevant check.
```

Good first prompts include four details:

- The outcome you want.
- The files, feature, or behavior you care about.
- What is out of scope.
- What verification should count as done.

For example:

```text
Fix the broken provider error message in the config loader. Do not change the
provider registry. Add a regression test and run only the config crate tests.
```

If you are not sure where the bug is, say that:

```text
Investigate why `codesmith doctor` reports the wrong provider. Do not edit
files yet. Return the likely cause, evidence, and a proposed patch plan.
```

CodeSmith works best when you let investigation and implementation happen in
separate steps for unfamiliar code. For small, well-understood changes, a
single implementation request is fine.

Next: [PRESETS.md](PRESETS.md) explains when to use Plan, Agent, and YOLO.

## 4. Understanding the Interface

The interactive REPL is a line-based loop (Claude Code / Codex style): you type
a line, the engine runs a turn, and output streams to the terminal.

- Status line: one line above the prompt with the running state, preset tier,
  context-window occupancy (`🧭`), the permission mode (`🚥`, see
  [Modes](#5-modes) below), the running sub-agent count (`🤖`), model,
  reasoning-effort tier (`🧠` — the `reasoning_effort` setting as `off` /
  `low` / `med` / `high` / `max`, or `auto`), and version. The model label
  prints `[providers.<id>] display_model`
  when you set one — the useful case is an alias gateway whose real backend is
  a different model — and otherwise the model id. The `🧭` denominator likewise
  honours the provider's `context_window` override before falling back to the
  static model table. The in-turn live line carries the mode, sub-agent count,
  and context percent too.
- Prompt: where you type prompts and slash commands (`/help` lists them).
- Transcript: the conversation, tool calls, command output summaries, and
  model responses, streamed as they happen.

The transcript is the audit trail. When CodeSmith reads files, runs commands,
or edits code, the action appears there. If a command fails, use the visible
failure output as part of your next instruction instead of starting over.

Tool calls print one collapsed line each (the call, its arguments, and the
first line of the result). To see the full result of a call that already
ran, use `/expand` (the last turn's calls), `/expand <n>` (one call), or
`/expand all`. Press `Ctrl+O` while a turn is streaming to switch between
collapsed and expanded rendering live. The terminal transcript is
append-only — expanding reprints the detail below the current output
rather than rewriting history, and keystrokes other than `Ctrl+O` typed
while a turn runs are discarded.

The prompt accepts normal prompts and slash commands; `/help` lists every
registered command. Typing `/` shows how many commands there are, `Tab`
completes the name (press it twice to list the candidates when several
match), and the dim text after the cursor previews the match with a one-line
description. `Shift+Tab` cycles the run mode (section 5).
Use an `@path` mention to attach an image file to the next message, or
`/attach <path>` for local media. `Ctrl+C` cancels a running turn; press it
twice in a row to quit.

Next: [CLI.md](CLI.md) is the command and subcommand reference.

## 5. Modes

CodeSmith has three visible modes:

| Mode | Use it for | Default posture |
| --- | --- | --- |
| Plan | Exploration, design, and review before changes | Read-only investigation |
| Agent | Normal multi-step coding work | Tool use with approval gates |
| YOLO | Trusted repos where you want automatic execution | Auto-approval and trust |

Switch modes in the REPL directly:

```text
/mode plan
/mode agent
/mode yolo
```

(`/plan`, `/agent`, and `/yolo` are shortcuts for the matching `/mode`
value; a bare `/mode` prints the current mode.) `Shift+Tab` at the prompt
cycles the same modes without typing a command — `agent → plan → yolo`, with
`coordinator` as a fourth stop while the experimental `coordinator_mode`
feature is enabled. The switch applies to the next message you send, and the
`🚥` segment of the status line above the prompt shows where you landed.

Plan mode is the safest place to start in an unfamiliar repository. It is for
inspection and decision-making, not file edits. It can open read-only
sub-agents to investigate several areas — or several repositories — in
parallel; each child is bounded to read-only tools, and a directory outside the
workspace has to be trusted with `/trust add <path>` first (`docs/SUBAGENTS.md`,
"Working across repositories").

Plan mode ends at an approval prompt of its own: `exit_plan_mode` prints the plan
body and asks. `[y]` approves it once and `[n]` (or an empty line) skips. Anything
else you type is feedback: it refuses the plan *and hands your sentence to the
running turn*, so the model revises the plan file and asks again — `[r] <feedback>`
is the same thing spelled out, and a bare `[r]` asks you for the words on the next
line. A refusal leaves plan mode active and the plan file in place, so revising
never means starting over. `[a]` is the one to avoid here: the plan tool has no
arguments to scope by, so "always" covers every later plan approval in the
session.

Approving the plan ends that turn. A Plan-mode turn's toolset registers no write
tools at all (`apply_patch`, `write_file`, and shell are absent), so the turn has
nothing left it can do — and the session is returned to the mode you came from,
which means your next message starts the implementation with those tools in
hand.

Agent mode is the default for most contribution work. It lets CodeSmith read,
run checks, and edit files while keeping risky actions behind approval gates.

YOLO mode is for trusted workspaces where you intentionally want the model to
act without stopping for approvals. Do not use it in a repository you do not
trust.

Modes are separate from model routing. `/mode` switches the visible mode,
while `/model auto` controls model and thinking selection for turns.

You can also change approval behavior from `/config` by editing the approval
mode. Use this only when you understand how it changes tool execution.

Next: [PRESETS.md](PRESETS.md) has the full preset, approval, and trust-mode
reference.

## 6. Slash Commands

Slash commands are typed at the prompt. They are useful when you want to
change CodeSmith state directly instead of asking the model in natural
language.

Common commands for first-time users:

| Command | Use |
| --- | --- |
| `/mode` | Switch between agent, plan, yolo, and coordinator modes (`/plan`, `/agent`, `/yolo` are shortcuts) |
| `/preset` | Show, switch, export, or drop the preset tier (`/preset simple`, `/preset list`, `/preset export <name>`, `/preset off`) |
| `/model` | Select a model or use `/model auto` |
| `/models` | List live models from the active endpoint |
| `/provider` | Pick the active API provider |
| `/config` | Edit runtime and provider settings |
| `/settings` | Inspect persistent UI preferences |
| `/skills` | List skills (`--remote` browses the community registry; `/skill install <source>` installs one) |
| `/memory` | Inspect the memory file (`# text` at the prompt quick-appends a note) |
| `/extension` / `/mods` | Manage extensions and script mods (list, install, activate, reload) |
| `/compact` | Summarize long context to recover token budget |
| `/review` | Ask for a structured review workflow |
| `/mcp` | Configure or inspect MCP server integration |

Live model discovery also works as the `codesmith models` subcommand.

Use `/provider` to switch the active provider. Provider IDs, environment
variables, model defaults, and capability notes are
kept in the provider registry document.

Use `/model auto` when you want CodeSmith to choose the model and thinking
level per turn. Use a fixed model when you need repeatable benchmarking or a
strict cost profile.

Use `/compact` when a session gets long and the model starts carrying too much
history. Compaction trades raw transcript detail for a concise working summary.

This guide intentionally does not list every command. The command surface
changes more often than the onboarding flow, and `/help` inside the REPL is
the source of truth while you are in a session.

Next: [CONFIGURATION.md](CONFIGURATION.md) covers runtime settings and
[MCP.md](MCP.md) covers Model Context Protocol integration.

## 7. Working with Tools

CodeSmith tools are structured actions. Instead of only producing prose, the
model can call tools to inspect and change the workspace.

Examples of tool-backed work include:

- Reading a file before explaining it.
- Searching for call sites before proposing a refactor.
- Running a focused test command.
- Applying a small patch.
- Opening a sub-agent for parallel investigation.

Tool use is governed by mode, approvals, and sandbox policy. The exact behavior
depends on the current mode and config, but the basic rule is simple: start in
Plan for read-only exploration, use Agent for normal changes, and reserve YOLO
for trusted automation.

The workspace boundary matters. CodeSmith is expected to work in the directory
you launched it from or the workspace you configured. Be explicit when a task
should stay inside a repo:

```text
Only inspect and edit files under this repository. Do not touch parent
directories or global config.
```

When a command needs network, writes outside the workspace, or a risky shell
operation, expect an approval prompt unless you have configured more permissive
behavior. Every approval prompt refuses with a reason: type `[r] <feedback>` — or
just type your sentence — and it refuses that one call and sends your text to the
running turn, so the model hears what to change instead of guessing why you said
no. Only the four keys (`y`/`n`/`a`/`e`) answer the question itself.

`[a]` remembers one **grouping key**, and the prompt spells out what that key
covers: `this command family` for shell commands (`git status` covers
`git status -s`, not `git push`), `each of these command families` for a chain
(`cargo build && cargo clippy` — a later call is covered only when *every* one of
its families was approved), `this exact call` for a command the safety analyzer
calls dangerous (`… | sh`, a multi-line script), `patches to the same paths` for a
patch, `requests to the same host` for a fetch, and `every <tool> call` for a tool
with no such family — one `write_file` answer covers its later calls, and one
`agent_open` answer covers every spawn for the rest of the run. So `[a]` on one
tool never silences a different tool's prompt; answer it once per tool you are
happy to stop being asked about. `[e]` is always the exact call: that one
invocation is denied and nothing else. `[a]` and `[e]` last until the process
exits; `/permissions allow <command prefix>` is the form written to
`~/.codesmith/approvals.toml` that survives a restart.

Good tool instructions are concrete:

```text
Run the narrowest test that covers this parser change. If it fails, report the
failure and stop before broadening the test scope.
```

Avoid asking for broad cleanup during a focused fix. Smaller tool scopes make
the transcript easier to review and the final diff easier to merge.

Next: [TOOL_SURFACE.md](TOOL_SURFACE.md) lists the tool surface and
[SANDBOX.md](SANDBOX.md) explains sandbox behavior.

## 8. Sub-agents and Parallel Work

Sub-agents are background child agents. The parent session gives a child a
focused task, receives an agent id, and can continue working while the child
runs.

The main orchestration tools are:

- `agent_open`: start a child with a task and role.
- `agent_eval`: wait for and collect the child result.
- `agent_close`: cancel a running child.

You normally do not need to call these tools directly. Ask for parallel work in
plain language:

```text
Open one read-only explorer for the config crate and another for the CLI
provider picker. Have both return file references and risks before we plan the
fix.
```

Useful roles include:

| Role | Good for |
| --- | --- |
| `general` | Multi-step tasks; the default when no role is specified |
| `explore` | Read-only code mapping |
| `plan` | Design and migration planning |
| `review` | Bug-focused review of an existing change |
| `implementer` | A tightly specified edit |
| `verifier` | Running checks and reporting pass/fail evidence |

Sub-agents are most useful when work can be separated cleanly. Do not use them
for tiny edits, and do not ask multiple agents to write the same files at the
same time.

Next: [SUBAGENTS.md](SUBAGENTS.md) covers roles, lifecycle, concurrency, and
output contracts.

## 9. Skills

Skills are reusable instruction packs. A skill is usually a `SKILL.md` file
that teaches CodeSmith how to perform a recurring workflow, use a tool family,
or follow a project convention.

Use skills when a task has a repeatable process:

- Reviewing a specific kind of PR.
- Working with a document or spreadsheet format.
- Following a team release checklist.
- Using a project-specific memory or wiki workflow.

Inside the REPL, `/skill` activates a skill when one is available, and `/skills`
lists installed skills.

Good skills are narrow. They should tell the model what workflow to follow,
what evidence to collect, and what to avoid. They should not hide credentials
or replace normal repository documentation.

If a repository has its own instructions, treat them as part of the active
work. Read the local guidance before editing, and keep any contribution within
the repository's conventions.

Next: the full skill reference — authoring, discovery directories,
conditional activation, and community installs — lives in
[SKILLS.md](SKILLS.md). The "Publishing Your Own Skill" section in
[README.md](../README.md) is the short version, and configuration details
are in [CONFIGURATION.md](CONFIGURATION.md).

## 10. Getting Help

Start with doctor output:

```bash
codesmith doctor
```

Use JSON when filing a detailed issue:

```bash
codesmith doctor --json
```

For authentication problems, check which source is winning: saved config,
keyring, environment, or an explicit launch flag. A stale `DEEPSEEK_API_KEY`
environment variable can override what you expected to use.

For provider problems, confirm the active provider and model:

```text
/provider
/model
```

For long or confusing sessions, use `/compact` to reduce context pressure, or
start a fresh session in the same workspace and summarize what you need.

When reporting an issue, include:

- CodeSmith version.
- Install method.
- Operating system and terminal.
- Provider and model.
- The exact command or prompt.
- Relevant doctor output.
- Whether the problem happens in a fresh workspace.

Do not paste API keys, private source code, or secrets into a public issue.

Next: [OPERATIONS_RUNBOOK.md](OPERATIONS_RUNBOOK.md) has operational triage and
recovery steps.

## FAQ

### Is CodeSmith only for DeepSeek?

No. DeepSeek is only the fallback when no provider is configured; every
shipped provider is first-class, hosted and local OpenAI-compatible endpoints
alike. Use `/provider` or `codesmith --provider <id>` to choose a provider.
Keep the provider registry open when configuring a route.

### Which mode should I use first?

Use Plan for unfamiliar code, Agent for normal implementation, and YOLO only
for trusted repositories where automatic execution is acceptable.

### Why does CodeSmith ask before running commands?

Approvals are part of the safety model. Shell commands, paid tools, writes, and
actions outside the expected workspace can have side effects. Approval prompts
let you keep control while still letting the model do useful work.

### How do I run a Python file on macOS?

Open Terminal in the folder that contains the file and run:

```bash
python3 your_file.py
```

If macOS says `python3` is missing, install Python from
[python.org](https://www.python.org/downloads/macos/) or with Homebrew:

```bash
brew install python
```

Inside CodeSmith, ask the agent to inspect the file and run it with
`python3 your_file.py`. If the script needs packages, install them in a virtual
environment first:

```bash
python3 -m venv .venv
source .venv/bin/activate
python3 -m pip install -r requirements.txt
python3 your_file.py
```

### Where is my config stored?

CodeSmith config lives at `~/.codesmith/config.toml`. Project overlays
can also affect behavior when a workspace config exists.

### How do I keep costs predictable?

Use `/model auto` for routing, choose a fixed model when you need a strict
profile, and compact long sessions. For larger tasks, ask CodeSmith to plan
before implementing so you do not spend tokens on the wrong path.

### How do I continue previous work?

CodeSmith saves sessions. Use the session picker or resume/continue CLI paths
documented in the README and modes guide. For a risky experiment, fork the
session before changing direction.

### What should I do when the model gets confused?

Stop and restate the goal, constraints, and current evidence. If the transcript
is long, use `/compact` or start a fresh session with a short handoff. If the
problem is operational, run `codesmith doctor` and inspect the reported config
and provider state.

### Should I put project rules in prompts or files?

Use repository files for durable project rules and prompts for turn-specific
intent. If a workflow repeats across projects, consider turning it into a
skill.

### Can CodeSmith edit files outside the current repository?

A file tool that targets a path outside the workspace does not simply fail — it
asks:

```text
⚠ path outside the workspace: /abs/sibling-repo (list_dir)
allow? [y] allow it now · [a] always allow it in this workspace (saved) · [n] skip:
```

`[y]` allows that path for the turn, `[a]` also remembers it for this workspace
(the list `/trust add <path>` writes — undo it with `/trust remove <path>`), and
`[n]` keeps the refusal. Persistent grants are recorded per workspace and are
read again on the next turn, so a trusted sibling repository keeps working
across restarts. Sub-agents ask the same way: a child that hits the boundary
raises the question in the session that spawned it.

Shell commands were never workspace-scoped for reads (`rg`, `git -C ../other`
already reach another checkout); the prompt governs the built-in file tools,
which must not follow a path the model invented. For contribution work, keep
instructions scoped to the current repository unless you intentionally need
something else.

### Where should I go after this guide?

Read the focused reference for the thing you are changing. For most users, the
next pages are install, configuration, providers, modes, tools,
and sub-agents.

Next: [INSTALL.md](INSTALL.md), [CONFIGURATION.md](CONFIGURATION.md),
[PROVIDERS.md](PROVIDERS.md), [PRESETS.md](PRESETS.md), and
[TOOL_SURFACE.md](TOOL_SURFACE.md).
