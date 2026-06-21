# Startup Trust Boundary Audit

Status date: 2026-06-20

This audit is deliberately non-destructive. No startup behavior is moved until
the current pre-trust/post-trust classification is reviewed and split into a
separate implementation change.

## Scope

This audit covers the startup and workspace-trust boundary for the active Rust
runtime:

- `crates/cli/src/lib.rs`
- `crates/tui/src/main.rs`
- `crates/tui/src/config.rs`
- `crates/tui/src/commands/config.rs`
- `crates/tui/src/tui/app.rs`
- `crates/tui/src/tui/onboarding/mod.rs`
- `crates/tui/src/tui/onboarding/trust_directory.rs`
- `crates/tui/src/tui/ui.rs`
- `crates/tui/src/hooks.rs`
- `crates/tui/src/tools/spec.rs`
- `crates/tui/src/workspace_trust.rs`

The audit focuses on when CodeSmith first reads or executes workspace-sensitive
inputs, and whether those actions happen before or after the user has trusted the
workspace.

## Summary

CodeSmith already has workspace trust concepts, but the current trust prompt is
primarily a TUI onboarding gate. It is not yet a hard startup pipeline boundary
that separates all safe pre-trust initialization from all project-sensitive
post-trust initialization.

First implementation slice status:

- The early `dotenv().ok()` call has been removed; interactive startup now uses
  explicit `workspace/.env` loading only when startup workspace initialization is
  allowed.
- Project config overlay is gated behind the same startup boundary; untrusted
  interactive workspaces do not read `$WORKSPACE/.codesmith/config.toml` before
  the trust prompt.
- `SessionStart` hooks are deferred while `OnboardingState::TrustDirectory` is
  visible and are fired once after the trust gate clears.

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
- Read through `is_workspace_trusted(workspace)`.
- Written through `save_workspace_trust(workspace)` and the onboarding trust
  prompt.
- Legacy workspace-local markers under `.deepseek/` are still accepted by
  `needs_trust(workspace)`.

This is a startup/onboarding decision. It is related to, but not identical to,
runtime `trust_mode`.

### Runtime `trust_mode`

Runtime `trust_mode` answers: "Should file tools bypass the normal workspace path
boundary during this session?"

- Carried through `App`, session state, and `ToolContext`.
- Enabled by YOLO mode and `/trust on`.
- Also set for the current session when the user accepts the onboarding trust
  prompt.
- In `ToolContext::resolve_path()`, `trust_mode == true` bypasses the usual
  workspace path check.

This is a broad capability switch and should not be confused with merely marking
a workspace as trusted for future onboarding.

### Per-workspace external path allowlist

The external path allowlist answers: "Which specific paths outside this workspace
may CodeSmith file tools access while `trust_mode` is false?"

- Managed by `/trust add <path>`, `/trust remove <path>`, and `/trust list`.
- Loaded through `WorkspaceTrust::load_for(workspace)`.
- Applied through `ToolContext::trusted_external_paths`.
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
| CLI dispatcher direct commands and TUI delegation | `crates/cli/src/lib.rs` | Before TUI runtime | Pre-trust safe for pure dispatch; command-specific behavior varies | Some delegated commands enter TUI without a shared startup boundary document. | Keep dispatcher behavior, but make TUI startup boundary the runtime source of truth. |
| Process hardening | `crates/tui/src/main.rs`, `crates/tui/src/sandbox/process_hardening.rs` | Very early in TUI `main()` | Pre-trust safe | Defensive process setup should not depend on workspace trust. | Keep pre-trust. |
| Panic hook / crash dump setup | `crates/tui/src/main.rs` | Very early in TUI `main()` | Pre-trust safe | May write diagnostic state, but does not read workspace-controlled startup input. | Keep pre-trust; ensure crash dumps avoid leaking secrets. |
| Signal cleanup task | `crates/tui/src/main.rs` | Very early in TUI `main()` | Pre-trust safe | Cleanup registration is process-scoped. | Keep pre-trust. |
| Workspace `.env` loading | `crates/tui/src/main.rs` | After workspace resolution and startup boundary calculation in interactive startup | Post-trust only / explicit bypass | The old `dotenvy::dotenv()` cwd search has been removed. Interactive startup now loads only `workspace/.env` and only when the workspace is already trusted or explicitly bypassed by YOLO/skip-onboarding. Non-interactive dotenv policy is still pending. | Keep explicit-path loading. Define non-interactive dotenv behavior in a follow-up slice. |
| CLI argument parsing | `crates/tui/src/main.rs` | Early TUI `main()` | Pre-trust safe | CLI args are user-provided process inputs, not repo-controlled files. | Keep pre-trust. |
| Global config loading | `crates/tui/src/main.rs`, `crates/tui/src/config.rs` | Before runtime dispatch | Pre-trust safe | User-owned global config can enable hooks or paths that later execute. It is still not workspace-controlled input. | Keep pre-trust; document that global config is trusted user input. |
| Logging setup | `crates/tui/src/main.rs` | Before command dispatch | Pre-trust safe | Logging sinks may include paths from user config. | Keep pre-trust if sourced only from user/CLI config. |
| Workspace resolution | `crates/tui/src/main.rs` | Early `run_interactive()` / command-specific paths | Pre-trust safe | Needed to decide trust state. | Keep pre-trust. |
| Workspace trust check | `crates/tui/src/config.rs`, `crates/tui/src/tui/onboarding/mod.rs` | During App/onboarding state construction | Pre-trust safe | Reads global trusted-workspace list and legacy marker paths. Legacy workspace markers are workspace-local inputs. | Keep for compatibility, but review whether legacy markers should be treated as sufficient trust. |
| Project config overlay from `$WORKSPACE/.codesmith/config.toml` or legacy `.deepseek/config.toml` | `crates/tui/src/main.rs` | In `run_interactive()` only when startup workspace initialization is allowed | Post-trust only / explicit bypass | Untrusted interactive workspaces no longer read project config before the trust prompt. The existing denylist remains a defense-in-depth check for trusted/bypassed project config. Runtime reload after accepting trust is not implemented in this slice. | Decide whether to reload project config after trust acceptance or apply it only on next launch. Update docs to match the denylist. |
| Config file creation/migration | `crates/tui/src/main.rs` | Before TUI launch | Pre-trust safe if user-state only | Writes user config/state and may migrate legacy config. | Keep pre-trust if no workspace-controlled input is applied. |
| System skill installation | `crates/tui/src/main.rs` | Before TUI launch | Pre-trust safe if bundled/global only | Installing bundled skills into user state is not workspace-controlled, but workspace skill discovery is separate. | Keep pre-trust for bundled skills; audit workspace-local skill discovery separately. |
| Workspace snapshot pruning | `crates/tui/src/main.rs` | Before TUI launch | Uncertain / requires review | Uses workspace path and deletes old snapshot metadata. It likely affects CodeSmith-managed state, but the workspace trust implication should be documented. | Classify exact storage target and keep only CodeSmith-owned cache cleanup pre-trust. |
| Spillover/truncate cache pruning | `crates/tui/src/main.rs` | Before TUI launch | Pre-trust safe if cache-only | CodeSmith-owned cache maintenance. | Keep pre-trust if cache paths are user-state paths. |
| Old session cleanup | `crates/tui/src/main.rs` | Before TUI launch | Pre-trust safe if state-only | CodeSmith-owned session/state maintenance. | Keep pre-trust if it does not read workspace-controlled session hooks/config. |
| App construction and onboarding state calculation | `crates/tui/src/tui/app.rs` | Before event loop | Pre-trust safe for state construction | Constructs the TUI state and determines whether to show `TrustDirectory`. | Keep pre-trust. |
| Hook executor construction | `crates/tui/src/tui/app.rs` | Before onboarding trust prompt is accepted | Pre-trust safe only if global config is the sole source | Hooks are user-configured, but later execution can run commands in the untrusted workspace context. | Construction can remain pre-trust; execution should be gated. |
| SessionStart hook execution | `crates/tui/src/tui/ui.rs` | Before event loop for trusted/bypassed startup; deferred while `TrustDirectory` is visible | Post-trust only / explicit bypass | The hook executor can still be constructed pre-trust from user config, but `SessionStart` execution is suppressed while the workspace trust prompt is active and fired once after the gate clears. | Keep the one-shot guard. Add broader message/tool hook guards only if a path can reach them during onboarding. |
| MessageSubmit hooks | `crates/tui/src/tui/ui.rs` | During user message dispatch | Post-trust only for workspace-gated sessions | Should not run while the workspace trust gate is active. | Confirm message dispatch is blocked during onboarding; add a guard if not already guaranteed. |
| Tool hooks | `crates/tui/src/tui/tool_routing.rs` | Around tool execution | Post-trust/tool-policy controlled | Tool calls should occur only after runtime trust and approval policy are active. | Keep under tool policy; document relationship to trust mode. |
| MCP config loading for counts/status | `crates/tui/src/tui/app.rs`, `crates/tui/src/mcp.rs` | During App construction | Pre-trust safe if sourced from global config | Project config currently denies `mcp_config_path`; global MCP config is user input. | Keep pre-trust for global config only; do not allow project MCP config pre-trust. |
| Workspace-local skills discovery | `crates/tui/src/tui/app.rs`, skills modules | During App construction / tool catalog build | Uncertain / requires review | Workspace-local skill metadata may be repo-controlled. Loading text is less risky than executing commands, but model-visible instructions can affect behavior. | Audit exact discovery and execution points. Prefer post-trust for workspace-local skills or label as constrained pre-trust read. |
| Tool path enforcement | `crates/tui/src/tools/spec.rs`, `crates/tui/src/core/engine.rs` | During tool execution | Post-trust/tool-policy controlled | `trust_mode` bypasses workspace path checks; allowlist narrows external access while untrusted. | Keep enforcement centralized in `ToolContext`; document the three trust concepts. |

## Findings

### Finding 1: `.env` loading happens before workspace trust

The current TUI runtime calls `dotenv().ok()` before CLI parsing and before the
workspace is resolved. This means the `.env` source is the process cwd, not
necessarily the `--workspace` path, and it is applied before CodeSmith can decide
whether the workspace is trusted.

This should be treated as post-trust initialization unless CodeSmith explicitly
chooses a different product rule for `.env`.

Recommended follow-up:

- Remove the global early `dotenv().ok()` call.
- Resolve the workspace first.
- Load `workspace.join(".env")` only after trust, or behind an explicit opt-in
  for non-interactive commands.

### Finding 2: Project config is currently a constrained pre-trust read

Interactive startup currently merges project config before the onboarding trust
prompt. The merge is partially constrained: sensitive fields such as `api_key`,
`base_url`, `provider`, and `mcp_config_path` are denied at project scope, and
some policy fields are constrained.

The boundary is still not explicit. Project config remains repo-controlled input
and can affect runtime behavior through allowed fields.

Recommended follow-up:

- Decide whether project config is post-trust only.
- If it remains pre-trust, document the exact allowed safe subset and ensure each
  field can only reduce capability before trust.
- Update configuration docs to match the implemented denylist.

### Finding 3: `SessionStart` hooks can run before trust acceptance

The hook executor is built during app construction. Building it from global
user-owned config is acceptable pre-trust. Executing `SessionStart` before the
user accepts the workspace trust prompt is more sensitive because hooks run with
workspace context.

Recommended follow-up:

- Do not execute `SessionStart` while onboarding is `TrustDirectory`.
- Fire it after trust acceptance, or define a separate restricted pre-trust hook
  event if that behavior is needed.

### Finding 4: The trust prompt is not yet the full startup boundary

The current workspace trust prompt controls the onboarding UI and current session
trust mode. It does not currently sit between all project-sensitive reads and all
runtime initialization. Several project-sensitive reads or actions may happen
before the prompt.

Recommended follow-up:

- Introduce explicit startup phases in code or documentation:
  1. process prelude
  2. CLI parse and global config
  3. workspace resolution and trust check
  4. trusted project initialization
  5. runtime dispatch

## Follow-up implementation candidates

These are implementation candidates, not changes made by this audit.

1. **Trust-aware `.env` loading**
   - Move `.env` loading after workspace resolution and trust acceptance.
   - Prefer `dotenvy::from_path(workspace.join(".env"))` over cwd search.
   - Define behavior for non-interactive `exec`, `serve`, and command modes.

2. **Project config split**
   - Split project config into `pre_trust_project_config_subset` and
     `post_trust_project_config`.
   - Only allow capability-tightening fields pre-trust.
   - Move instructions, notes paths, and other behavior-shaping fields
     post-trust unless explicitly approved.

3. **Hook execution gate**
   - Defer `SessionStart` until onboarding is complete.
   - Add a guard to message/tool hook paths if any can be reached while the trust
     prompt is active.

4. **Startup phase helpers**
   - Consider extracting named helpers such as:
     - `init_process_pre_trust()`
     - `parse_cli_and_load_user_config()`
     - `resolve_workspace_trust()`
     - `init_project_post_trust()`
     - `dispatch_runtime()`

5. **Docs cleanup**
   - Update `docs/CONFIGURATION.md` to describe actual `.env` timing/source and
     project config denylist.
   - Update `docs/MODES.md` so `/trust` no-arg is status, `/trust on` enables
     runtime trust mode, and `/trust add` is the narrower external path option.

## Verification checklist

Before changing startup behavior:

1. Add or update tests proving untrusted workspace `.env` is not loaded before
   trust.
2. Add or update tests proving project config cannot relax approval, sandbox,
   shell, provider, key, or endpoint behavior before trust.
3. Add or update tests proving `SessionStart` hooks do not run while the
   workspace trust prompt is active.
4. Verify trusted workspace persistence still suppresses the onboarding prompt
   according to product expectations.
5. Verify `/trust on`, `/trust off`, `/trust add`, `/trust remove`, and
   `/trust list` still map to their intended runtime/file-tool semantics.
6. Smoke-test interactive startup, `exec`, `serve --mcp`, `serve --http`, and
   `serve --acp` after any startup refactor.

## Status

P0's first-stage deliverable is complete when this audit is reviewed and linked
from the architecture parity RFC. Runtime behavior changes should be tracked as
separate follow-up work items derived from the candidates above.
