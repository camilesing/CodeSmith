# Plan 03: CLAUDE.md Tiered Trust + @include

**Finding:** 1 (CLAUDE.md four-tier trust model + `@include` depth limit)
**Status:** Implemented
**Depends on:** none
**Blocks:** none

## Context

Claude Code loads memory/config files (CLAUDE.md) from four trust tiers with
different priorities — Managed (`/etc/...`), User (`~/.claude/...`), Project
(`{cwd}/CLAUDE.md`), Local (`.claude/rules/*.md`) — and supports `@include
<path>` with a hard `MAX_INCLUDE_DEPTH = 5`, symlink dedup, and an exclude
list. CodeSmith loads only two tiers (Project + User global) via
`crates/agent-runtime/src/project_context.rs`, picks the first matching project
file (`break` at `:425`), has no Managed tier, no Local rules glob, and no
`@include` support at all (zero matches for `@include`/`MAX_INCLUDE_DEPTH`).

## Deliverables

### 1. New module `crates/agent-runtime/src/claudemd.rs`

`project_context.rs` is already 1288 lines; a dedicated module keeps the new
tier machinery separate. Registered via `pub mod claudemd;` in `lib.rs`.

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MemoryTier { Managed, User, Project, Local }
impl MemoryTier { pub fn as_label(self) -> &'static str { ... } }

pub const MAX_INCLUDE_DEPTH: usize = 5;

/// Scan content for `^@include\s+(.+)$` lines; expand `~/` and env via
/// workspace_trust::expand_path (workspace_trust.rs:23-45).
fn parse_includes(content: &str, base: &Path) -> Vec<PathBuf>

/// Read + recursively @include-expand a single memory file.
/// - canonicalize_or_keep (workspace_trust.rs:18) for symlink-stable dedup
///   into `processed`
/// - depth >= MAX_INCLUDE_DEPTH or already-processed → silent empty return
/// - matches `excludes` → silent empty return
/// - size cap MAX_CONTEXT_SIZE per resolved file
fn process_memory_file(
    path: &Path,
    tier: MemoryTier,
    processed: &mut HashSet<PathBuf>,
    excludes: &[PathBuf],
    depth: usize,
) -> Vec<(MemoryTier, String)>

/// /etc/codesmith/CLAUDE.md on unix; home fallback mirroring
/// tui/config.rs:2532-2547 default_managed_config_path.
fn load_managed_tier() -> Option<(MemoryTier, String)>

/// Reuse project_context.rs:40-45 global candidates as the User tier.
fn load_user_tier(home: &Path) -> Option<(MemoryTier, String)>

/// First match of project_context.rs:27-34 candidates (with parent walk),
/// reusing the existing parent-walk behavior.
fn load_project_tier(workspace: &Path) -> Option<(MemoryTier, String)>

/// Glob .claude/rules/*.md + .codesmith/rules/*.md via ignore::WalkBuilder
/// (reuse working_set.rs:305-313 discovery_walk_builder shape; depth 1;
/// sorted). Each file read via process_memory_file so @include works in rules.
fn load_local_tier(workspace: &Path) -> Vec<(MemoryTier, String)>

/// Collect Managed → User → Project → Local, merge with
/// `<!-- tier: ... -->` labels (generalize project_context.rs:538-551
/// merge_global_and_project_instructions from 2 tiers to N).
pub fn load_all_memory_tiers(
    workspace: &Path,
    home: Option<&Path>,
    excludes: &[String],
) -> String
```

### 2. Wiring

In `load_project_context_with_parents_and_home`
(`crates/agent-runtime/src/project_context.rs:460`), replace the
`merge_global_and_project_instructions` call at `:495-499` with
`claudemd::load_all_memory_tiers(workspace, home, &excludes)`. Set
`ctx.instructions` to the merged result. Keep `ctx.source_path` pointing at the
most-specific project file (existing behavior) and `ctx.is_trusted =
check_trust_status(workspace)` unchanged.

### 3. Config surface

- `MemoryConfig` (`crates/tui/src/config.rs:592-610`): add
  `#[serde(default)] pub excludes: Option<Vec<String>>`.
- Engine mirror (`crates/agent-runtime/src/engine_config.rs:135-138`): add
  `pub memory_excludes: Vec<String>`, default `[]` at `:234-237`.
- Thread `memory_excludes` through to the loader call site.

### 4. Tests (tempdir style, matching `project_context.rs:774+`)

- Four tiers present → all four blocks in priority order with labels.
- `@include` resolves an external file inline.
- Depth limit: 5 nested includes load; the 6th is silently truncated.
- Cycle `a → b → a` does not infinite-loop (dedup).
- Symlink dedup: the same file reached via `/tmp` and `/private/tmp` loads once.
- Exclude list drops a matched path.
- Local rules glob loads multiple `*.md` files in sorted order.
- Managed tier absent when the `/etc` file is missing.

## Risk

Behavior change: the system prompt will now contain up to four merged tiers
plus `@include`-expanded content, where it previously held at most global +
one project file. Mitigations:

- The merge is additive; priority preserves "project overrides global".
- `[memory].excludes` is the escape hatch.
- Update `docs/MEMORY.md` to document the four tiers, `@include`, the depth
  limit, and the exclude list.

## Stop rules

- Do not change `ProjectContext::as_system_block` (`project_context.rs:135`)
  or its `<project_instructions source="...">` wrapper contract.
- Do not remove the legacy `merge_global_and_project_instructions` if other
  callers exist; check call sites before replacing.
- Do not load workspace-local skills or other repo-controlled executable
  content — only memory markdown files.

## Files

- `crates/agent-runtime/src/claudemd.rs` (new)
- `crates/agent-runtime/src/lib.rs` (module registration)
- `crates/agent-runtime/src/project_context.rs` (`:460`, `:495-499`)
- `crates/agent-runtime/src/engine_config.rs` (`:135-138`, `:234-237`)
- `crates/tui/src/config.rs` (`:592-610`)
- `docs/MEMORY.md` (update)
