# Startup Trust Boundary Audit

Status date: 2026-06-20. Updated 2026-10-12: re-pointed at the current tree
after the ratatui TUI removal (2026-10-11, commits f88f4443/b47af98c) — the
line-REPL in `crates/cli` is the only frontend — and Findings reconciled with
the implemented startup boundary.

This audit is deliberately non-destructive. No startup behavior is moved until
the current pre-trust/post-trust classification is reviewed and split into a
separate implementation change.

## Scope

This audit covers the startup and workspace-trust boundary for the active Rust
runtime (the single `codesmith` binary in `crates/cli`):

- `crates/cli/src/main.rs` (thin shim: `run_cli()`)
- `crates/cli/src/lib.rs` (CLI dispatcher, `run_cli`)
- `crates/cli/src/main_root.rs` (the startup spine: CLI args, panic hook,
  signal cleanup, workspace resolution, the startup trust boundary
  (`WorkspaceInitBoundary`), `.env` loading, project-config gating, serve /
  exec entry points)
- `crates/cli/src/interactive/` (line-REPL front-end: state, command dispatch
  including `/trust` in `commands/config_cmds.rs`)
- `crates/cli/src/hooks.rs` and `crates/agent-runtime/src/hooks.rs` (hook
  executor and `HookEvent` enum)
- `crates/cli/src/mcp.rs` (MCP config loading)
- `crates/cli/src/workspace_trust.rs` (per-workspace external path allowlist)
- `crates/agent-runtime/src/workspace_trust.rs` (persisted workspace trust)
- `crates/agent-runtime/src/onboarding.rs` (onboarding marker, `needs_trust`)
- `crates/agent-runtime/src/tools/spec.rs` (tool path enforcement)
- `crates/agent-runtime/src/sandbox/process_hardening.rs`

Audit history: the original 2026-06-20 scope pointed at the ratatui TUI
(`crates/tui/src/main.rs`, `tui/app.rs`, `tui/onboarding/`, `tui/ui.rs`,
`tui/config.rs`, …). Those files were removed with the TUI on 2026-10-11;
this audit now tracks the CLI/REPL front-end that replaced them.

The audit focuses on when CodeSmith first reads or executes workspace-sensitive
inputs, and whether those actions happen before or after the user has trusted the
workspace.

## Summary

CodeSmith has workspace trust concepts, and since the audit's original date an
explicit startup boundary (`WorkspaceInitBoundary` in
`crates/cli/src/main_root.rs`) has been implemented: interactive startup
resolves the workspace and the trust boundary first, and workspace-controlled
initialization inputs (`.env`, project config, telemetry flush) are gated
behind it. The former TUI onboarding gate was removed with the ratatui TUI on
2026-10-11; the line-REPL has no onboarding-state deferral (the onboarding
marker is bookkeeping only — see Trust concepts below), so the boundary is now
carried entirely by `resolve_workspace_init_boundary` and its callers.

Current status:

- Interactive startup loads `workspace/.env` explicitly via
  `dotenvy::from_path(workspace.join(".env"))`, only when startup workspace
  initialization is allowed (`load_workspace_dotenv_if_allowed`,
  `crates/cli/src/main_root.rs:4072`–`4094`, called at `:5409` after boundary
  resolution at `:5407`). No cwd-based `dotenv().ok()` call exists.
- Project config overlay is gated behind the same startup boundary
  (`should_load_project_config` `:4065`–`4069`, `merge_project_config` defined
  at `:5255`, applied at `:5422`–`5423`); untrusted interactive workspaces do
  not read `$WORKSPACE/.codesmith/config.toml` before the trust decision, and
  the project-scope denylist (`:5297`–`5350`) remains a defense-in-depth check
  for trusted/bypassed project config.
- The TUI mechanism that deferred `SessionStart` hooks while
  `OnboardingState::TrustDirectory` was visible is gone with the TUI. In the
  current tree `HookEvent::SessionStart` exists in the enum
  (`crates/agent-runtime/src/hooks.rs:18`) but no production path fires it, so
  there is nothing to defer; `OnboardingState::TrustDirectory` survives only
  as stale comments (`crates/agent-runtime/src/engine/host_executor.rs:16942`,
  `:16948`).

Remaining highest-priority follow-ups are:

1. Decide whether project config should dynamically reload after trust acceptance
   or only apply on the next launch.
2. Define non-interactive trust policy for `exec --auto`, `serve --mcp`,
   `serve --http`, and `serve --acp`.
3. Revisit whether `--skip-onboarding` should continue to bypass startup
   workspace initialization.
4. Document the difference between persisted workspace trust, runtime
   `trust_mode`, and the per-workspace external path allowlist in user-facing
   docs.

## Trust concepts

CodeSmith currently has three related but distinct trust concepts.

### Persisted workspace trust / onboarding trust

Persisted workspace trust answers: "Should this workspace show the onboarding
trust prompt again?"

- Stored in the global config as `[projects."<workspace>"].trust_level =
  "trusted"`.
- Read through `is_workspace_trusted(workspace)`
  (`crates/agent-runtime/src/workspace_trust.rs:100`; level parsing at
  `:79`–`:99`).
- Written through `save_workspace_trust(workspace)`
  (`crates/agent-runtime/src/workspace_trust.rs:142`). Note: the TUI trust
  prompt was its interactive writer; after the TUI removal the function has no
  production caller (test-only in `crates/cli/src/config.rs:5617`), so
  persisted trust is currently established by editing config or via the
  workspace-local markers below.
- Workspace-local markers under `.codesmith/` (`trusted`, `trust.json`) are
  also accepted by `needs_trust(workspace)`
  (`crates/agent-runtime/src/onboarding.rs:39`–`46`).

This is a startup/onboarding decision. It is related to, but not identical to,
runtime `trust_mode`. With the TUI onboarding UI gone, the onboarding marker
(`~/.codesmith/.onboarded`, `crates/agent-runtime/src/onboarding.rs:19`–`35`)
is bookkeeping only: clean starts silently mark onboarding
(`crates/cli/src/main_root.rs:5535`–`5548`) and `/logout` removes the marker
(`crates/cli/src/interactive/commands/mod.rs:826`–`837`); no interactive
prompt drives it.

### Runtime `trust_mode`

Runtime `trust_mode` answers: "Should file tools bypass the normal workspace path
boundary during this session?"

- Carried through `ReplState` (`crates/cli/src/interactive/state.rs`), session
  state, and `ToolContext` (`trust_mode` field,
  `crates/agent-runtime/src/tools/spec.rs:173`).
- Enabled by YOLO mode and `/trust on`
  (`crates/cli/src/interactive/commands/config_cmds.rs:255`–`281`).
- The former "accepting the onboarding trust prompt also sets session
  trust_mode" path was TUI onboarding behavior and is gone with it.
- In `ToolContext::resolve_path()` (`crates/agent-runtime/src/tools/spec.rs:526`),
  `trust_mode == true` bypasses the usual workspace path check.

This is a broad capability switch and should not be confused with merely marking
a workspace as trusted for future onboarding.

### Per-workspace external path allowlist

The external path allowlist answers: "Which specific paths outside this workspace
may CodeSmith file tools access while `trust_mode` is false?"

- Managed by `/trust add <path>`, `/trust remove <path>`, and `/trust list`.
- Loaded through `WorkspaceTrust::load_for(workspace)`
  (`crates/cli/src/workspace_trust.rs:52`).
- Applied through `ToolContext::trusted_external_paths`
  (`crates/agent-runtime/src/tools/spec.rs:204`).
- Grants access only through CodeSmith file tools; it does not loosen the shell
  OS sandbox.

## Classification rules

| Classification | Meaning |
|---|---|
| Pre-trust safe | The action does not read or execute workspace-controlled input and can happen before workspace trust. |
| Constrained pre-trust | The action reads workspace-sensitive input before trust, but code currently constrains dangerous fields or side effects. The constraint must be documented and reviewed. |
| Post-trust only | The action reads, executes, or applies workspace-controlled input and should happen only after a trusted-workspace decision. |
| Uncertain / requires review | The action's data source, side effects, or trust implications are not clear enough from the current audit. |

## Startup action audit

| Startup action | Location | Current phase | Classification | Risk | Recommended action |
|---|---|---|---|---|---|
| CLI dispatcher direct commands and interactive delegation | `crates/cli/src/lib.rs` (`run_cli` `:418`); interactive path continues in `crates/cli/src/main_root.rs` | Before the REPL starts | Pre-trust safe for pure dispatch; command-specific behavior varies | Delegated commands enter the runtime without a shared startup boundary document outside `run_interactive`. | Keep dispatcher behavior, but make the `main_root.rs` startup boundary the runtime source of truth. |
| Process hardening | `crates/agent-runtime/src/sandbox/process_hardening.rs` | No longer invoked at startup | Uncertain / requires review | `apply_process_hardening()` (`:53`) survives but has no production caller after the TUI removal — the TUI `main()` was its only call site. | Decide whether the CLI should call it before the Tokio runtime, or retire it. |
| Panic hook / crash dump setup | `crates/cli/src/main_root.rs:937` (`std::panic::set_hook`) | Very early in startup | Pre-trust safe | May write diagnostic state, but does not read workspace-controlled startup input. | Keep pre-trust; ensure crash dumps avoid leaking secrets. |
| Signal cleanup task | `crates/cli/src/main_root.rs:471` (`spawn_signal_cleanup_task`) | Very early in startup | Pre-trust safe | Cleanup registration is process-scoped. | Keep pre-trust. |
| Workspace `.env` loading | `crates/cli/src/main_root.rs:4072`–`4094` (`load_workspace_dotenv_if_allowed`), called at `:5409` | After workspace resolution and startup boundary calculation (`resolve_workspace_init_boundary`, `:5407`) in interactive startup | Post-trust only / explicit bypass | Interactive startup loads only `workspace/.env` via `dotenvy::from_path`, and only when the workspace is already trusted or explicitly bypassed by YOLO/skip-onboarding. Non-interactive dotenv policy is still pending. | Keep explicit-path loading. Define non-interactive dotenv behavior in a follow-up. |
| CLI argument parsing | `crates/cli/src/main_root.rs` (`Cli` args struct) | Early startup | Pre-trust safe | CLI args are user-provided process inputs, not repo-controlled files. | Keep pre-trust. |
| Global config loading | `crates/cli/src/main_root.rs`, `crates/cli/src/config.rs` | Before runtime dispatch | Pre-trust safe | User-owned global config can enable hooks or paths that later execute. It is still not workspace-controlled input. | Keep pre-trust; document that global config is trusted user input. |
| Logging setup | `crates/cli/src/logging.rs`, wired from `main_root.rs` | Before command dispatch | Pre-trust safe | Logging sinks may include paths from user config. | Keep pre-trust if sourced only from user/CLI config. |
| Workspace resolution | `crates/cli/src/main_root.rs` | Early `run_interactive()` / command-specific paths | Pre-trust safe | Needed to decide trust state. | Keep pre-trust. |
| Workspace trust check | `crates/agent-runtime/src/workspace_trust.rs`, `crates/agent-runtime/src/onboarding.rs` (`needs_trust` `:39`–`46`), consumed by `resolve_workspace_init_boundary` (`main_root.rs:4053`) | During startup boundary resolution | Pre-trust safe | Reads the global trusted-workspace list and workspace-local marker paths. Workspace markers are workspace-local inputs. | Keep; review whether workspace markers should be treated as sufficient trust. |
| Project config overlay from `$WORKSPACE/.codesmith/config.toml` | `crates/cli/src/main_root.rs:5255` (`merge_project_config`), gated at `:5422`–`5423` | In `run_interactive()` only when startup workspace initialization is allowed | Post-trust only / explicit bypass | Untrusted interactive workspaces do not read project config before the trust decision. The denylist (`:5297`–`5350`) remains a defense-in-depth check for trusted/bypassed project config. Runtime reload after accepting trust is not implemented. | Decide whether to reload project config after trust acceptance or apply it only on next launch. Update docs to match the denylist. |
| Config file creation/migration | `crates/cli/src/main_root.rs:5458`–`5467` (`ensure_config_file_exists`) | Before the REPL starts | Pre-trust safe if user-state only | Writes user config/state. | Keep pre-trust if no workspace-controlled input is applied. |
| System skill installation | `crates/cli/src/main_root.rs:5488`–`5492` (`install_system_skills`) | Before the REPL starts | Pre-trust safe if bundled/global only | Installing bundled skills into user state is not workspace-controlled, but workspace skill discovery is separate. | Keep pre-trust for bundled skills; audit workspace-local skill discovery separately. |
| Workspace snapshot pruning | `crates/cli/src/main_root.rs:5493`–`5499` (`prune_workspace_snapshots`) | Before the REPL starts | Uncertain / requires review | Uses workspace path and deletes old snapshot metadata. It likely affects CodeSmith-managed state, but the workspace trust implication should be documented. | Classify exact storage target and keep only CodeSmith-owned cache cleanup pre-trust. |
| Spillover/truncate cache pruning | `crates/cli/src/main_root.rs:5501`–`5520` (`truncate::prune_older_than`) | Before the REPL starts | Pre-trust safe if cache-only | CodeSmith-owned cache maintenance. | Keep pre-trust if cache paths are user-state paths. |
| Old session cleanup | `crates/cli/src/main_root.rs:5522`–`5526` (`cleanup_old_sessions`) | Before the REPL starts | Pre-trust safe if state-only | CodeSmith-owned session/state maintenance. | Keep pre-trust if it does not read workspace-controlled session hooks/config. |
| Onboarding marker bookkeeping | `crates/agent-runtime/src/onboarding.rs:19`–`35`; `crates/cli/src/main_root.rs:5535`–`5548` | After the startup boundary, before the REPL starts | Pre-trust safe | The former TUI onboarding-state calculation (`App` construction deciding whether to show `TrustDirectory`) is gone; clean starts silently mark onboarding and `/logout` removes the marker (`interactive/commands/mod.rs:826`–`837`). | Keep pre-trust; the marker no longer gates anything interactive. |
| REPL state construction | `crates/cli/src/interactive/state.rs` (`ReplState`) | Before the REPL event loop | Pre-trust safe for state construction | Constructs the line-REPL state; no onboarding trust screen exists to show. | Keep pre-trust. |
| Hook executor construction | `crates/cli/src/interactive/state.rs:251` (`HookExecutor::new`) | Before the REPL starts | Pre-trust safe only if global config is the sole source | Hooks are user-configured, but later execution can run commands in the untrusted workspace context. | Construction can remain pre-trust; execution should be gated. |
| SessionStart hook execution | no production firing site (event defined in `crates/agent-runtime/src/hooks.rs:18`; the TUI `ui.rs` firing site was removed 2026-10-11) | n/a | Post-trust only / explicit bypass (currently unreachable) | Nothing fires `SessionStart` in the current tree, so the former "defer while `TrustDirectory` visible" guard is moot. Re-introduce the gate if a firing site returns. | Re-add a trust gate if `SessionStart` gains a caller again. |
| MessageSubmit hooks | `crates/cli/src/hooks.rs:328` (`execute_message_submit_transform`), reached through the `HookHost` trait during user message dispatch | During user message dispatch in the REPL | Post-trust only for workspace-gated sessions | The REPL only dispatches messages after startup; with the onboarding gate gone there is no in-REPL trust screen during which a message could be submitted. | Confirm message dispatch cannot run before startup completes; add a guard if a firing path appears. |
| Tool hooks | `crates/agent-runtime/src/callback_bridge.rs:259` / `:306` (`ToolCallBefore` / `ToolCallAfter`) | Around tool execution | Post-trust/tool-policy controlled | Tool calls should occur only after runtime trust and approval policy are active. | Keep under tool policy; document relationship to trust mode. |
| MCP config loading for counts/status | `crates/cli/src/mcp.rs`, wired from `main_root.rs` (`mcp_config_path` consumers at `:1973`, `:2247`, `:2766`) | During startup / REPL construction | Pre-trust safe if sourced from global config | Project config currently denies `mcp_config_path`; global MCP config is user input. | Keep pre-trust for global config only; do not allow project MCP config pre-trust. |
| Workspace-local skills discovery | `crates/agent-runtime/src/skills/mod.rs` (registry), `crates/cli/src/skills/` | During tool catalog build | Uncertain / requires review | Workspace-local skill metadata may be repo-controlled. Loading text is less risky than executing commands, but model-visible instructions can affect behavior. | Audit exact discovery and execution points. Prefer post-trust for workspace-local skills or label as constrained pre-trust read. |
| Tool path enforcement | `crates/agent-runtime/src/tools/spec.rs` (`resolve_path` `:526`), reached via the `crates/cli/src/core/engine.rs` construction bridge | During tool execution | Post-trust/tool-policy controlled | `trust_mode` bypasses workspace path checks; allowlist narrows external access while untrusted. | Keep enforcement centralized in `ToolContext`; document the three trust concepts. |

## Findings

### Finding 1: `.env` loading happens before workspace trust — RESOLVED

Original finding (2026-06-20): the then-current TUI runtime called
`dotenv().ok()` before CLI parsing and before the workspace was resolved, so
the `.env` source was the process cwd, not necessarily the `--workspace`
path, and it was applied before CodeSmith could decide whether the workspace
was trusted.

Resolution: no `dotenv().ok()` call exists in the tree. The recommendation is
implemented — `load_workspace_dotenv_if_allowed`
(`crates/cli/src/main_root.rs:4072`–`4094`) loads
`dotenvy::from_path(workspace.join(".env"))` only when the startup boundary
allows workspace initialization, and it is called at `:5409` after
`resolve_workspace_init_boundary(&workspace, cli.skip_onboarding, startup_yolo)`
at `:5407`. Project-config gating rides the same boundary
(`should_load_project_config` `:4065`–`4069` / `merge_project_config` applied
at `:5421`–`5423`). Non-interactive dotenv behavior (exec/serve) remains the
open tail of this finding.

### Finding 2: Project config is currently a constrained pre-trust read — largely resolved at the gate level

Original finding (2026-06-20): interactive startup merged project config
before the onboarding trust prompt, partially constrained by a field denylist.

Current state: the merge is now gated behind the startup boundary — untrusted
interactive workspaces do not read `$WORKSPACE/.codesmith/config.toml` at all
before the trust decision (`should_load_project_config` /
`merge_project_config`, `crates/cli/src/main_root.rs:5421`–`5423`). For
trusted or explicitly bypassed workspaces the denylist still rejects
project-scope `api_key` / `base_url` / `provider` / `mcp_config_path` and
constrains policy fields (`:5297`–`5350`, rationale recorded as #417) as
defense-in-depth.

Recommended follow-up:

- Decide whether project config is post-trust only for non-interactive paths
  too (`exec`, `serve`), where the boundary currently resolves from flags.
- Keep the denylist documented and in sync with configuration docs.
- Decide whether project config reloads after a trust decision taken
  mid-session (currently: next launch only).

### Finding 3: `SessionStart` hooks can run before trust acceptance — moot after the TUI removal

Original finding (2026-06-20): the hook executor was built during app
construction and `SessionStart` could execute before the user accepted the
workspace trust prompt.

Current state: the TUI onboarding gate and its `TrustDirectory` state machine
were removed on 2026-10-11. `HookEvent::SessionStart` remains in the enum
(`crates/agent-runtime/src/hooks.rs:18`) but no production path fires it in
the current tree (verified: only the enum, its `as_str` mapping, and tests
reference it), so there is nothing left to defer. The tool-path hooks that do
fire (`ToolCallBefore`/`ToolCallAfter`, `crates/agent-runtime/src/callback_bridge.rs:259`/`:306`)
run around tool execution, which only happens after startup and under tool
policy.

Recommended follow-up:

- If `SessionStart` gains a production firing site again, execute it only
  after the startup boundary allows workspace initialization (the
  `WorkspaceInitBoundary` check is the natural gate).

### Finding 4: The trust prompt is not yet the full startup boundary — partially resolved

Original finding (2026-06-20): the workspace trust prompt controlled the
onboarding UI and current-session trust mode but did not sit between all
project-sensitive reads and all runtime initialization.

Current state: an explicit startup boundary now exists —
`WorkspaceInitBoundary` (`crates/cli/src/main_root.rs:4042`–`4063`) is
resolved from persisted trust / `--skip-onboarding` / YOLO and gates `.env`
loading, the project-config merge, and telemetry flushing
(`attach_telemetry_if_trusted`). With the interactive trust prompt gone, the
boundary is computed, not prompted. Remaining gaps: non-interactive entry
points (`exec --auto`, `serve --mcp`/`--http`/`--acp`) resolve trust from
flags/persistence without an explicit documented policy, and
`apply_process_hardening()` is no longer called by any startup path (see the
startup action table).

Recommended follow-up:

- Keep the boundary as the single seam and document the startup phases it
  implements:
  1. process prelude
  2. CLI parse and global config
  3. workspace resolution and trust boundary
  4. trusted project initialization
  5. runtime dispatch
- Define the non-interactive trust policy (Summary follow-up 2).

## Follow-up implementation candidates

These are implementation candidates, not changes made by this audit.

1. **Trust-aware `.env` loading** — IMPLEMENTED
   - `load_workspace_dotenv_if_allowed` loads
     `dotenvy::from_path(workspace.join(".env"))` behind the startup
     boundary (`crates/cli/src/main_root.rs:4072`–`4094`, called `:5409`).
   - Still open: behavior for non-interactive `exec`, `serve`, and command
     modes.

2. **Project config split**
   - Split project config into `pre_trust_project_config_subset` and
     `post_trust_project_config`.
   - Only allow capability-tightening fields pre-trust.
   - Move instructions, notes paths, and other behavior-shaping fields
     post-trust unless explicitly approved.
   - Current state note: the interactive path already reads no project config
     pre-trust; this candidate now mostly concerns non-interactive paths.

3. **Hook execution gate** — MOOT as written (2026-10-12)
   - The TUI onboarding screen whose visibility was to gate `SessionStart` no
     longer exists, and no production path fires `SessionStart` in the
     current tree. Re-open this candidate if `SessionStart` regains a firing
     site.

4. **Startup phase helpers**
   - Consider extracting named helpers such as:
     - `init_process_pre_trust()`
     - `parse_cli_and_load_user_config()`
     - `resolve_workspace_trust()`
     - `init_project_post_trust()`
     - `dispatch_runtime()`
   - Current state note: `resolve_workspace_init_boundary` already plays the
     `resolve_workspace_trust()` role in `main_root.rs`.

5. **Docs cleanup**
   - Update `docs/CONFIGURATION.md` to describe actual `.env` timing/source and
     project config denylist.
   - Update `docs/PRESETS.md` so `/trust` no-arg is status, `/trust on` enables
     runtime trust mode, and `/trust add` is the narrower external path option.

## Verification checklist

Before changing startup behavior:

1. Add or update tests proving untrusted workspace `.env` is not loaded before
   trust (existing guards: `load_workspace_dotenv_if_allowed` tests at
   `crates/cli/src/main_root.rs:7421`–`7429`;
   `should_load_project_config` tests at `:7374`–`7390`).
2. Add or update tests proving project config cannot relax approval, sandbox,
   shell, provider, key, or endpoint behavior before trust.
3. If `SessionStart` gains a production firing site, add tests proving it does
   not run while the startup boundary blocks workspace initialization.
4. Verify trusted-workspace persistence still routes startups past the
   boundary without re-prompting (there is no interactive prompt in the
   line-REPL; the marker at `~/.codesmith/.onboarded` is bookkeeping only).
5. Verify `/trust on`, `/trust off`, `/trust add`, `/trust remove`, and
   `/trust list` still map to their intended runtime/file-tool semantics
   (`crates/cli/src/interactive/commands/config_cmds.rs:255`–`281`).
6. Smoke-test interactive startup, `exec` (`--auto` flag at
   `crates/cli/src/main_root.rs:340`), `serve --mcp`, `serve --http`, and
   `serve --acp` (flags at `:684`–`698`, single-mode check at `:761`) after
   any startup refactor.

## Status

P0's first-stage deliverable is complete when this audit is reviewed and linked
from the architecture parity RFC. Runtime behavior changes should be tracked as
separate follow-up work items derived from the candidates above.
