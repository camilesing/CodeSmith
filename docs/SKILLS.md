# Skills

A skill is a reusable instruction pack: a directory containing a `SKILL.md`
file that teaches CodeSmith how to perform a recurring workflow, use a tool
family, or follow a project convention. Skills are declarative Markdown — no
code to compile, no registration step. Drop a directory with a `SKILL.md`
into any discovery location and the next session picks it up automatically.

CodeSmith uses progressive disclosure to keep context small: the session
prompt only lists each skill's name, description, and path (capped at 12,000
characters). The full `SKILL.md` body is loaded on demand — by the model via
the `load_skill` tool, or by you via the `/skill` command.

This document is the complete reference: discovery locations, the
`SKILL.md` format, activation paths, commands, community installs, and
troubleshooting.

## Quick start

1. Create a directory for your skill:

   ```bash
   mkdir -p ~/.codesmith/skills/release-checklist
   ```

2. Write a `SKILL.md` with YAML frontmatter:

   ```markdown
   ---
   name: release-checklist
   description: Use when cutting a release — walks the full v4 release checklist and gates each step on verification.
   ---

   # Release Checklist

   1. Run `cargo test --workspace` and confirm zero failures.
   2. Update CHANGELOG.md with the new version and today's date.
   3. ... (your steps here)
   ```

3. Restart the TUI (or start a new session) and verify:

   ```text
   /skills
   ```

   Your skill now appears in the model-visible skills catalog, in the `/`
   completion menu, and can be invoked with `/skill release-checklist` or
   simply `/release-checklist`.

## Discovery directories and precedence

CodeSmith scans the following directories, in order, and merges everything it
finds. Only directories that exist are scanned; when two skills share the
same frontmatter `name`, the first match wins.

| # | Scope | Directory |
|---|---|---|
| 1 | workspace | `<workspace>/.agents/skills` |
| 2 | workspace | `<workspace>/skills` |
| 3 | workspace | `<workspace>/.opencode/skills` |
| 4 | workspace | `<workspace>/.claude/skills` |
| 5 | workspace | `<workspace>/.cursor/skills` |
| 6 | workspace | `<workspace>/.codesmith/skills` |
| 7 | global | `~/.agents/skills` |
| 8 | global | `~/.claude/skills` |
| 9 | global | `~/.codesmith/skills` |
| 10 | global | `~/.codesmith/skills` (legacy fallback) |

The cross-tool locations (`.agents`, `.opencode`, `.claude`, `.cursor`) mean
skills you already maintain for other agents are reused as-is — no
duplication or symlinking needed.

Notes on how directories are walked:

- **Nested layouts are fine.** Any directory containing a `SKILL.md` is one
  skill and the walk does not descend past it, so you can organize by vendor
  or category — `<root>/<vendor>/<skill>/SKILL.md` — up to a maximum depth
  of 8. Hidden subdirectories (`.git`, caches) are skipped; symlinked
  directories are followed.
- **Custom install directory.** The top-level `skills_dir` config key
  (default `~/.codesmith/skills`) is where `/skill install` places new
  skills. If you point it somewhere outside the standard set, it is appended
  to the discovery list. See [CONFIGURATION.md](CONFIGURATION.md).

## The SKILL.md format

A `SKILL.md` is a YAML frontmatter block followed by a Markdown body. Only
`name` is required.

```markdown
---
name: my-skill
description: One sentence telling the model when this skill applies.
when_to_use: Trigger hints for the model, shown alongside the description.
allowed-tools: read_file, list_dir, exec_shell
paths:
  - "docs/**/*.md"
version: 1.0.0
---

# My Skill

Instructions for the agent go here.
```

### Frontmatter fields

| Field | Required | Meaning |
|---|---|---|
| `name` | yes | Skill identifier; used for `/skill <name>` lookup and conflict resolution. |
| `description` | recommended | One-liner the model reads in the catalog to decide relevance. Write it as "use this when …". |
| `when_to_use` | no | Extra trigger hints, surfaced next to the description. |
| `allowed-tools` | no | Comma-separated or YAML-list tool names the skill expects to use. Both `allowed-tools` and `allowed_tools` spellings are accepted. |
| `model` | no | Preferred model for runs using this skill. |
| `effort` | no | Preferred thinking-effort level. |
| `user-invocable` | no | `false` hides the skill from `/skills`, the `/` menu, and direct invocation — it stays model-selectable only. Default `true`. |
| `paths` | no | Path globs that conditionally activate the skill (see below). |
| `version` | no | Version string; used by the bundled-skill upgrader and community installs. |
| `context` | no | Context hint for execution. |
| `agent` | no | Sub-agent role hint (see [SUBAGENTS.md](SUBAGENTS.md)). |
| `shell` | no | Shell preference for snippets. |

Parsing details worth knowing:

- List fields accept comma-separated strings, YAML block lists, or
  `[a, b]` inline lists. Long descriptions can use YAML block scalars
  (`>`, `|`, with `>-`/`|+` chomping).
- **Plain-Markdown fallback:** a file with no `---` frontmatter fence is
  still accepted — the first `# Heading` becomes the name and the whole file
  is the body. Useful for quick notes, but an explicit `description` makes
  auto-selection far more reliable.

### Conditional activation with `paths`

When a skill declares `paths`, CodeSmith matches the globs against the
files in the current working set and, on match, injects the skill into that
turn as a *Matched Conditional Skill* — even if the model never asked for
it. Glob semantics are gitignore-style: `/`-separated segments, `*` and `?`
within a segment, `**` across segments (for example `docs/**/*.md`).

This is the right tool for "whenever we touch files under `ops/`, follow the
runbook" skills.

### Writing good skill bodies

- **Be narrow.** One workflow per skill. A skill that tries to cover
  everything gets selected for nothing.
- **Tell the model what evidence to collect and what to avoid**, not just
  what to do.
- **Keep bodies self-contained.** The body is injected verbatim when the
  skill activates; it should not assume the user's message provides context
  it could state itself.
- Do not put secrets in skills — they are plain files, often synced through
  git or installed from third parties.

## How skills activate

There are four activation paths; all of them work for any discovered skill
without further setup.

1. **Model auto-selection.** The session prompt carries a `## Skills`
   catalog (name, description, path). When your task matches a description,
   the model calls the `load_skill` tool to read the full body, then follows
   it for the rest of the turn.
2. **Conditional path matching.** Skills with a `paths:` frontmatter are
   injected automatically when working-set files match (see above).
3. **Manual invocation.** `/skill <name>` activates a skill on your next
   message; any user-invocable skill is also directly callable as
   `/<skill-name>` — skills join the slash-command namespace after native
   and user-defined commands — and all of them appear in the `/` completion
   menu. An activated skill's rendered body is prepended to your next
   message as the turn's instruction.
4. **HTTP runtime API.** `GET /v1/skills` lists skills and
   `POST /v1/skills/{name}` enables or disables one (persisted to
   `~/.codesmith/skills_state.toml`). See [RUNTIME_API.md](RUNTIME_API.md).

## Command reference

| Command | Effect |
|---|---|
| `/skills` | List locally discovered skills (with parse warnings, if any). |
| `/skills <prefix>` | Filter the local list by name prefix. |
| `/skills --remote` | Browse the curated community registry instead of local skills. |
| `/skills sync` | Pull the registry index and download every curated skill to the local cache. |
| `/skill <name>` | Activate a skill for your next message. |
| `/skill new` | Start the bundled `skill-creator` skill to scaffold a new one. |
| `/skill install <source>` | Install a community skill (see below). |
| `/skill update <name>` | Refresh an installed skill from its source. |
| `/skill uninstall <name>` | Remove an installed skill. |
| `/skill trust <name>` | Mark a skill trusted, unlocking its shell snippets. |

## Installing community skills

`/skill install <source>` accepts three source forms:

| Source form | Resolves to |
|---|---|
| `github:owner/repo` | `https://github.com/<owner>/<repo>` archive (branch `main`, `master` fallback on 404). |
| `https://github.com/owner/repo` | Same as above — bare repo URLs are detected. |
| any other `http(s)://…` URL | Used directly as a tarball URL. |
| anything else | A lookup key in the curated registry. |

Related configuration, under the `[skills]` table in
`~/.codesmith/config.toml`:

```toml
[skills]
registry_url = "https://raw.githubusercontent.com/camilesing/codesmith-skills/main/index.json"
max_install_size_bytes = 5_242_880   # 5 MiB default
```

Installs are network-gated by the `[network]` policy (`github.com` and
`raw.githubusercontent.com` must be reachable; the default `prompt` mode
asks once and can persist). Downloaded archives land in
`~/.codesmith/cache/skills/` and are unpacked into your skills directory,
with a `.installed-from` marker recording the source for later
`/skill update`.

### Trust and shell snippets

A skill's body may embed shell snippets in two forms: fenced blocks opening
with <code>```!</code> and inline <code>!`command`</code> backtick spans.
For an **untrusted** skill these are replaced at load time with a disabled
placeholder — the model sees that the snippet exists but cannot run it.
Running `/skill trust <name>` drops a `.trusted` marker file next to the
`SKILL.md`; trusted snippets are then passed through with instructions to
execute via the `exec_shell` tool, so normal approval and sandbox policy
still apply. Trust is per-skill and deliberate: review what a community
skill wants to run before trusting it. MCP-prompt skills (see below) never
get shell snippets enabled.

## Companion files

Everything next to a `SKILL.md` — helper scripts, templates, reference
docs — is part of the skill. When the model loads a skill via `load_skill`,
it also receives a listing of these companion files (nested subdirectories
excluded) so it can open the ones it needs. This is the standard way to
ship a script a skill depends on:

```text
my-skill/
├── SKILL.md
├── extract_metrics.py
└── template.md
```

## Bundled system skills

First launch installs a set of bundled skills into `~/.codesmith/skills`
(or legacy `~/.codesmith/skills`): `skill-creator`, `delegate`,
`v4-best-practices`, `plugin-creator`, `skill-installer`, `mcp-builder`,
`documents`, `presentations`, `spreadsheets`, `pdf`, and `feishu`. The
bundles are versioned: upgrades add newly introduced skills but never
recreate one you deliberately deleted.

`/skill new` activates `skill-creator`, which walks you through authoring a
well-formed skill. MCP servers that expose prompts also surface them as
skills (marked as MCP-sourced); see [MCP.md](MCP.md).

## Troubleshooting

- **Skill doesn't appear in `/skills`.** Check the directory is one of the
  discovery locations above (and exists), and that the `SKILL.md` filename
  is exact. `/skills` prints parse warnings — a missing `name` in
  frontmatter (with no `# Heading` fallback) is the most common cause.
- **Wrong skill wins a name conflict.** Discovery is first-match-wins in the
  table order — a workspace `.agents/skills` skill shadows a global
  `~/.codesmith/skills` skill with the same `name`. Rename one of them, or
  disable the other via `POST /v1/skills/{name}`.
- **Shell snippets show "disabled until trusted".** Run
  `/skill trust <name>` after reviewing the skill.
- **A skill activates too eagerly.** Sharpen the `description` (this is what
  the model selects on) or gate it with `paths:` so it only appears for
  matching files.
- **Skill visible to the model but not in the `/` menu.** It is marked
  `user-invocable: false`, disabled in `~/.codesmith/skills_state.toml`, or
  MCP-sourced.

For the `[skills]` / `skills_dir` config surface see
[CONFIGURATION.md](CONFIGURATION.md); for skill internals see
[ARCHITECTURE.md](ARCHITECTURE.md). Compiled Rust extensions (tools,
commands, event handlers) are a separate mechanism covered in
[EXTENSIONS.md](EXTENSIONS.md).
