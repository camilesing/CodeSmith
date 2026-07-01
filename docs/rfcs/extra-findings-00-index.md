# RFC: Claude Code Extra-Findings Parity (Chapter 6)

**Issue:** TBD
**Status:** Implemented
**Date:** 2026-06-30
**Umbrella for:** Plans 01–06

## Context

This umbrella RFC tracks the implementation of the six deep-mechanism gaps
identified by comparing CodeSmith against
`/Users/camile/Work/TypeScript/claude-code-analysis/analysis/06-extra-findings.md`.

The six findings are:

1. CLAUDE.md tiered trust model (4 tiers + `@include` depth limit)
2. Trust timing: telemetry init after trust
3. Unicode steganography / hidden-character defense
4. Swarm global state bridge (`contextModifier` batch-collect-then-apply)
5. Privacy coupling proactive cutoff:
   - 5a `AnalyticsMetadata_I_VERIFIED_...` type barrier
   - 5b log-layer sanitization (tool results before transcript)
   - 5c session ID (ephemeral) instead of user ID
6. Summary (no implementation work — captured by the table below)

The gap analysis preceding this RFC confirmed two load-bearing facts that
reshape the plans:

- **CodeSmith ships no external telemetry sink by design.** Findings 2, 5a,
  and 5c only matter relative to a sink. Per the approved scope, we build the
  defensive scaffolding *and* a minimal **local-only** jsonl telemetry sink so
  the gating/type-barrier/ephemeral-id have something concrete to protect
  (Plan 05).
- **All shared state handles are already `Arc<Mutex<...>>`.** Child→parent
  shared-state回流 is already atomic via `HostServices`. The `contextModifier`
  closure queue is redundant for shared state; it is only meaningful for the
  by-value `ToolContext` fields. Per the approved scope, we still implement
  full literal parity (Plan 04): document the Arc回流, add a by-value回流
  channel, re-introduce child-permission narrowing (`restrictToSubset`), and
  plumb a `contextModifier` queue for concurrent batches.

## Plan documents

| Plan | Finding | Document | Status |
| --- | --- | --- | --- |
| 01 | 3 | `extra-findings-01-unicode-sanitization.md` | Implemented |
| 02 | 5b | `extra-findings-02-tool-result-scrubbing.md` | Implemented |
| 03 | 1 | `extra-findings-03-claudemd-tiered-trust.md` | Implemented |
| 04 | 4 | `extra-findings-04-swarm-state-bridge.md` | Implemented |
| 05 | 2 + 5a + 5c | `extra-findings-05-telemetry-scaffolding.md` | Implemented |
| 06 | 2 + 5a + 5c (follow-up) | `extra-findings-06-telemetry-emission-routing.md` | Implemented |

Status values: `Pending` / `In Progress` / `Implemented` / `Blocked`.

## Execution order

`01 → 02 → 03 → 04 → 05 → 06`

- 01 is a dependency of 02 (reuses the sanitization module).
- 03 is self-contained but a behavior change (system-prompt content grows).
- 04 is the largest and most behavioral (reverses the v0.6.6 full-inheritance
  design); placed late so earlier safety slices land first.
- 05 is the most speculative (telemetry scaffolding for a sink that did not
  exist); placed last. **06 closes 05's recorded deviations** — it wires
  the sink into the engine so `emit()` actually fires, honors the
  project-config `telemetry` flag post-merge, and sanitizes the diagnostic
  fields left as `String` in 5.2-apply.

Each plan is independently testable and checkpointed with
`cargo build -p <crate>` + `cargo test -p <crate>`. The umbrella status table
above is updated after each checkpoint.

## Out of scope

- Any external/networked telemetry sink. The Plan 05 sink is local jsonl only.
- Rewriting CodeSmith in TypeScript or renaming modules to match Claude Code.
- Treating Claude Code's implementation as the only acceptable architecture.

## References

- Reference analysis:
  - `/Users/camile/Work/TypeScript/claude-code-analysis/analysis/06-extra-findings.md`
- Related RFCs and audits:
  - `docs/rfcs/claude-code-architecture-parity.md`
  - `docs/STARTUP_TRUST_BOUNDARY_AUDIT.md`
