# Plan 04: Swarm State Bridge — Full Claude Code Parity

**Finding:** 4 (multi-agent global state bridge / `contextModifier`)
**Status:** Implemented
**Depends on:** none
**Blocks:** none

## Context

Claude Code propagates child→parent state via a `contextModifier` closure
(`{ toolUseID, modifyContext }`) collected during a concurrent tool batch and
applied atomically after the batch completes (`toolOrchestration.ts`
`queuedContextModifiers`). Children also inherit a deep-copy snapshot of the
parent's `ToolUseContext` with a **restricted subset** of parent permissions
(no escalation).

CodeSmith's exploration surfaced a load-bearing fact: **all shared state
handles are already `Arc<Mutex<...>>`** — `SharedTodoList`, `SharedPlanState`,
`SharedGoalState`, `SharedTeamContext`, `SharedTaskV2Manager`,
`SharedWorktreeSessionState` (`crates/agent-runtime/src/engine_config.rs:101-188`),
bridged to tools via `HostServices` (`host_services.rs:649`) and
`RuntimeToolServices` (`spec.rs:39-58`). Child mutations to these are already
atomically visible to the parent with no queue needed. The `contextModifier`
closure queue is therefore redundant for shared state; it is only meaningful
for the **by-value, non-Arc `ToolContext` fields** (`workspace`, `cwd`,
`trust_mode`, `auto_approve`, `features`, `state_namespace`, `network_policy`,
memory paths at `spec.rs:144-214`) which are cloned per-child at
`subagent/mod.rs:730` and do not回流.

Per the approved scope, this plan implements **full literal parity**: document
the Arc回流, add a by-value回流 channel, re-introduce child-permission
narrowing, and plumb a `contextModifier` queue for concurrent batches.

## Sub-slices

### 4.1 Document the Arc回流

Add a module-level doc comment to `crates/tui/src/tools/subagent/mod.rs`
stating that the `Shared*` handles are `Arc<Mutex<...>>` and that child→parent
shared-state回流 is already atomic via `HostServices`/`RuntimeToolServices` —
this IS the回流 equivalent of Claude Code's `contextModifier` for shared
state. Add a test proving a child's `Todo` mutation is visible to the parent
without any explicit回流 call.

### 4.2 By-value回流 channel

- `crates/agent-runtime/src/subagent.rs:467-477` (`SubAgentCompletion`): add
  `pub context_patch: Option<ContextPatch>`.
- New struct (in `subagent.rs`):
  ```rust
  #[derive(Clone, Default)]
  pub struct ContextPatch {
      pub auto_approve: Option<bool>,
      pub trust_mode: Option<bool>,
      // extend with other by-value fields as needed
  }
  ```
- Parent drain sites `turn_loop.rs:1368` and `:1494`
  (`subagent_completion_runtime_message` call): collect each batch's
  `context_patch` values into a queue during the `try_recv` drain loop, then
  apply them **once after the loop** (mirrors `queuedContextModifiers`).
- Apply rule: **tighten only, never loosen** — a child may set
  `auto_approve = Some(false)` (tighten) but `Some(true)` (loosen) is rejected
  and `tracing::warn!`-logged. This preserves the "child cannot escalate"
  spirit.

### 4.3 Child-permission narrowing (`restrictToSubset`) — reverses v0.6.6

> **Implementation note (done).** The original "add a `parent_tools: &[String]`
> (or registry handle) parameter" sketch did not fit the architecture: the
> child-construction site (`SubAgentToolRegistry::new` / `build_allowed_tools`)
> has **no parent tool-registry handle** — the child rebuilds a fresh surface
> via `ToolRegistryBuilder::with_full_agent_surface`, the same builder the
> parent uses. The faithful semantic (`child ⊆ parent effective`, where subset
> includes equality) is achieved instead by threading an
> `Option<Vec<String>>` **child-subset basis** through `SubAgentRuntime`:
>
> - `SubAgentRuntime.child_subset_basis` (`None` = unrestricted parent; `Some(set)`
>   = children must be ⊆ `set`) + `inherit_full_registry: bool`, both propagated
>   via `child_runtime()` / `background_runtime()`.
> - `SubAgentToolRegistry::new` sets `runtime.child_subset_basis` to THIS agent's
>   own effective `allowed_tools` (post-augmentation) before the builder clone,
>   so the spawn-tool family (`AgentSpawnTool`) carries it to grandchildren.
> - `build_allowed_tools` takes `parent_basis: Option<&[String]>` +
>   `inherit_full_registry: bool`; the default branch returns the parent's basis
>   when narrowed, and the explicit branch intersects with it. Top-level
>   `general` children keep the full surface (engine basis = `None`) → recursion
>   preserved; a narrowed `custom` parent's grandchildren inherit the narrow set
>   → no escalation.
> - The finding's *baseline* `toolPermissionContext` parity (tool families) was
>   already structurally enforced because parent and child use the same builder;
>   4.3 closes the residual CodeSmith-specific gap (narrowed parent's grandchild
>   re-expansion).

- `build_allowed_tools` (`crates/tui/src/tools/subagent/mod.rs`): takes
  `parent_basis: Option<&[String]>` + `inherit_full_registry: bool`; intersects
  the child's surface with the parent's effective set — the child becomes a
  subset and cannot escalate.
- Default: **parity on** (subset). Config
  `[subagents].inherit_full_registry: bool` (default `false`) is the escape
  hatch to restore the legacy full-inheritance behavior.
- Updated the `#[deprecated]` note on `SubAgentType::allowed_tools`
  (`crates/agent-runtime/src/subagent.rs`).
- Tests: `build_allowed_tools_*` (default inherits parent basis; explicit
  intersected; escape hatch skips both; Custom-empty still errors),
  `intersect_tool_names_*`, `child_runtime_propagates_inherit_full_registry_and_basis`,
  and `config::tests::subagent_inherit_full_registry_default_and_explicit`.

### 4.4 `contextModifier` queue for concurrent batches

- `execute_parallel_tool` (`crates/agent-runtime/src/engine/tool_execution.rs:156`):
  extend the return channel so each parallel outcome can carry an optional
  `ContextPatch` alongside its `ToolResult`.
- `turn_loop.rs:1966` (after the `FuturesUnordered` for one batch drains):
  collect the batch's patches and apply them once after the batch (atomic).
  Redundant for Arc-shared state (which is already atomic) but literal parity
  with the reference's batch-collect-then-apply pattern. Concurrent tools are
  read-only today, so patches will usually be `None` — but the plumbing exists.

## Risk

4.3 is a behavior change: children's tool sets shrink by default, which can
break existing flows that rely on sub-agents having the full parent registry.
Mitigations:

- The `[subagents].inherit_full_registry = true` flag restores the old default.
- Call out the change in `CHANGELOG.md` and `docs/SUBAGENTS.md`.
- 4.2 and 4.4 patches tighten only — they cannot escalate.

## Stop rules

- Do not remove the `CancellationToken::child_token()` cascading-abort path
  (`subagent/mod.rs:745`) — only the回流 and permission surface change.
- Do not loosen capability via a `ContextPatch` — tighten only.
- Do not change the `<codesmith:subagent.done>` sentinel format
  (`subagent/mod.rs:4487-4506`) — the text回流 path stays as-is; the patch
  rides alongside on `SubAgentCompletion`.

## Files

- `crates/tui/src/tools/subagent/mod.rs` (`:6239`, `:4383`, `:4460`,
  module doc)
- `crates/agent-runtime/src/subagent.rs` (`:430`, `:467-477`, `:128-138`)
- `crates/agent-runtime/src/engine/turn_loop.rs` (`:1368`, `:1494`, `:1966`)
- `crates/agent-runtime/src/engine/tool_execution.rs` (`:156`)
- `crates/agent-runtime/src/tools/spec.rs` (`ToolContext` reference)
- `crates/agent-runtime/src/engine_config.rs` (new config field)
- `crates/tui/src/config.rs` (`[subagents]` table)
- `docs/SUBAGENTS.md`, `CHANGELOG.md`
