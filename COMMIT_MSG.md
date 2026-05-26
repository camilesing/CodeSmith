# Commit Message — SlopLedger (#2127)

## Summary

Add a durable `SlopLedger` that makes invisible architectural residue
visible and queryable across agent sessions.

Closes: https://github.com/Hmbown/CodeWhale/issues/2127

## Problem

AI agents often leave behind invisible "slop" after a task:
compatibility shims, unmigrated callers, duplicated concepts,
naming drift, stale docs/tests, suspected dead code, and tool gaps.

Currently these residues are untracked. The next agent rediscovers
them, amplifies them, or mistakes them for intended architecture.

## Solution

A persistent JSON-backed ledger (`~/.codewhale/slop_ledger/slop_ledger.json`)
with four model-callable tools and a `/slop` slash command.

### Data Model

- **10 classification buckets**: retained_compatibility, unmigrated_callers,
  duplicate_concepts, naming_drift, stale_docs, stale_tests,
  suspected_dead_code, unverified_public_behavior, tool_gaps, accepted_debt
- **Severity**: critical | high | medium | low | info
- **Confidence**: certain | high | medium | low
- **Status lifecycle**: open → in_progress → resolved | accepted | wontfix
- Each entry carries: owner, source links, title, description,
  cleanup recommendation, timestamps, and optional task_id / thread_id

### Tools (model-callable)

| Tool | Description |
|---|---|
| `slop_ledger_append` | Append entries with bucket, severity, confidence, title, description |
| `slop_ledger_query` | Query with bucket/severity/status/text filters |
| `slop_ledger_update` | Update entry status |
| `slop_ledger_export` | Export as Markdown for handoffs / GitHub issues |

### Slash Command

- `/slop` — print summary
- `/slop query` — list entries
- `/slop export` — Markdown export
- Alias: `/canzha`

### Files Changed

| File | Change |
|---|---|
| `crates/tui/src/slop_ledger.rs` | **New** — 1089 lines |
| `crates/tui/src/main.rs` | +1: mod declaration |
| `crates/tui/src/tools/registry.rs` | +16: builder method |
| `crates/tui/src/core/engine/tool_setup.rs` | +1: registration |
| `crates/tui/src/commands/mod.rs` | +10: command + dispatch |
| `crates/tui/src/commands/config.rs` | +41: handler |

### Tests

8 unit tests: bucket roundtrip, save/load, query by bucket/search,
update status, markdown export, empty ledger, summary counts.

## How to Test

```bash
cargo test -p codewhale-tui -- slop_ledger
```

In TUI: `/slop`, `/slop query`, `/slop export`
