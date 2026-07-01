# Plan 06: Telemetry Emission Routing + Deviation Closure

**Issue:** follow-up to Plan 05
**Findings:** 2 + 5a + 5c (closure of Plan 05 deferrals)
**Status:** Implemented — slices 6.1, 6.2, 6.3 complete and tested; Plan 05 deviations closed.
**Depends on:** Plan 05 (5.1–5.4 implemented)

## Context

Plan 05 landed the telemetry scaffolding: a local-only `TelemetrySink`
(`~/.codesmith/telemetry/events.jsonl`), a `VerifiedAnalyticsMetadata` type
barrier, an ephemeral `telemetry_session_id` + `DEEPSEEK_THREAD_ID` split, a
`telemetry` config flag, and a trust-timed `attach()` in `run_interactive`.

Three deviations were recorded in Plan 05's Implementation notes and are
**not yet closed**:

1. **Emission routing deferred (5.4).** The sink is constructed and attached
   but is never handed to the engine, so no `emit()` calls occur. The
   scaffolding is in place but telemetry writes nothing. This is the
   load-bearing gap.
2. **Project-config `telemetry` flag not honored.** The sink's `enabled`
   flag is resolved from the **user** config at construction time (pre-trust,
   pre-merge). The project-config overlay is merged only post-trust, so a
   `telemetry = true` set in `$WORKSPACE/.codesmith/config.toml` is not
   reflected by the sink.
3. **Diagnostic fields left as `String` (5.2-apply).**
   `CapacityIntervention.replay_outcome` and
   `CapacityMemoryPersistFailed.error` were deliberately not wrapped in
   `VerifiedAnalyticsMetadata` because they can embed tool-output summaries
   and IO-error text (code/paths). They are currently carried raw. The type
   barrier's spirit asks that untrusted content be sanitized before it enters
   a "verified" field.

A fourth item — the five-helper startup extraction sketched in Plan 05
(`init_process_pre_trust` / `init_project_post_trust` / `dispatch_runtime`)
— is **out of scope** for this plan: the locked decision to keep the sink in
`run_interactive` scope removed its justification, and the post-trust block
is not self-contained (its locals flow into `run_tui`).

Candidate coverage gaps in Plans 01–04 were verified as **already closed**:

- Unicode sanitization is applied at MCP args (`mcp.rs:3013`), tool-result
  compaction (`engine/context.rs:307`), and the error path
  (`engine/turn_loop.rs:2577`) — parity with Claude Code; user-typed input
  is intentionally not sanitized.
- `SessionStart` hooks are suppressed while the trust gate is visible
  (`tui/src/tui/ui.rs:252-263` + `tui/src/tui/ui/tests.rs:1414`).
- Tool results always pass through `compact_tool_result_for_context`, which
  sanitizes before compaction.

## Deliverables

### 6.1 Emission routing (the core)

Thread a `TelemetrySink` handle from `run_interactive` into the engine so the
three capacity-event construction sites actually emit JSON.

- **Threading path.** Prefer an `Option<TelemetrySink>` field on the
  **Engine** struct (set via a setter or constructor param), **not** on
  `EngineConfig` — `EngineConfig` has ~30 construction sites (mostly tests),
  and an Engine field localizes the touch. `TelemetrySink` is `Clone`
  (Arc-backed), so cloning a handle into the engine is cheap; the
  `run_interactive`-owned original keeps living until session end.
- **Emit sites** (all in `crates/agent-runtime/src/engine/capacity_flow.rs`):
  - `CapacityDecision` (~line 322)
  - `CapacityIntervention` (~line 364)
  - `CapacityMemoryPersistFailed` (~line 952)
- **JSON shape.** The capacity `Event` types derive only `Debug, Clone` (no
  serde, by design). Do **not** add `Serialize` to them. Instead, at each
  emit site build a `serde_json::json!({ ... })` from the event's
  already-constructed fields (`session_id.as_str()`, `turn_id.as_str()`,
  `risk_band.as_str()`, `action.as_str()`, `reason.as_str()`, …) and call
  `if let Some(sink) = &self.telemetry_sink { sink.emit(value); }`.
  Centralize the shape in one private helper
  (`fn emit_capacity_event(sink, kind, fields)`) so the three sites share it
  and the emitted schema is auditable in one place.
- **Gating.** `TelemetrySink::emit` already consults `enabled` and
  `is_attached` (queue-pre-attach, write-post-attach, no-op when disabled),
  so call sites need no conditional beyond the `Option`.
- **`replay_outcome`/`error`.** For 6.1, emit these as-is (raw `String`);
  6.3 closes the sanitization gap.

### 6.2 Project-config `telemetry` flag honored

- Add `TelemetrySink::set_enabled(&self, enabled: bool)` (the `enabled` field
  is `Arc<AtomicBool>`; store with `store`). This does not weaken the
  construction barrier — `new_skeleton` is still the only constructor.
- In `run_interactive`, after the project-config merge produces the final
  `config`, call `telemetry_sink.set_enabled(config.telemetry_enabled())`.
  This makes a project-config `telemetry = true` take effect even though the
  sink was constructed pre-merge. Pre-trust queueing is unaffected: `enabled`
  only gates whether `emit` writes/queues, not whether the sink exists.

### 6.3 Diagnostic-field sanitization (type-barrier closure)

- Introduce `RedactedAnalyticsMetadata` in
  `crates/agent-runtime/src/telemetry.rs`: a newtype over `String`,
  `Debug, Clone, PartialEq, Eq`, constructed **only** via `redact(s: &str)`
  (no `From<String>`). `redact` applies `partially_sanitize_unicode`
  (control-char/zero-width/bidi/Tag-block strip) plus a best-effort
  path-redaction (replace `/…` path-like runs and backtick-quoted code spans
  with `<redacted>`).
- Convert `CapacityIntervention.replay_outcome: Option<String>` and
  `CapacityMemoryPersistFailed.error: String` to the redacted type. Construct
  via `RedactedAnalyticsMetadata::redact(...)` at the build site.
- Add `Display` (delegates to inner) so TUI `format!("{replay_outcome:?}")`
  keeps working.
- Rationale: `VerifiedAnalyticsMetadata::verified` asserts "this string is
  already safe" (ids, enums, controlled vocab).
  `RedactedAnalyticsMetadata::redact` asserts "I sanitized untrusted
  content". Both are non-`From<String>`. The two-type split makes the safety
  posture of each field explicit instead of leaving diagnostics as raw
  `String`.

## Tests

- 6.1: a `TelemetrySink` backed by a tempdir path; drive a capacity
  decision through the engine with telemetry enabled + attached; assert the
  jsonl file contains one line whose `action`/`risk_band`/`session_id`
  match the event. A second assertion with telemetry disabled → file
  absent (or unchanged).
- 6.2: construct a sink pre-merge with `enabled=false`, call
  `set_enabled(true)` post-merge, assert `is_enabled()` flipped and a
  subsequent `emit` writes.
- 6.3: `redact("output_mismatch: original='/Users/x/secret.rs' replay='…'")`
  → the path is replaced and zero-width chars stripped; round-trip via
  `Display`. Confirm no `From<String>` exists for either metadata type.

## Risk

- Threading the sink into the Engine is the widest touch in this plan. Keep
  it to one field + one setter; do not refactor `EngineConfig`'s ~30 sites.
- Do not add `Serialize` to the capacity `Event` types (intentionally
  serde-free). Build JSON at the emit site.
- `set_enabled` must not enable writes when the sink has no path
  (`emit` already guards: queue-only without a path).
- Do not change `VerifiedAnalyticsMetadata`'s construction barrier.
- The `codesmith-tool-impls` TTY test
  (`background_tty_command_has_controlling_terminal`) is a pre-existing
  flake under workspace-wide parallelism; do not touch it, and do not treat
  its flaky failure as a regression.

## Stop rules

- Stop at `cargo build -p codesmith-agent-runtime -p codesmith-tui` clean.
- Stop at `cargo test -p codesmith-agent-runtime -p codesmith-tui` green.
- Do not touch `codesmith-tool-impls`.

## Files

- `crates/agent-runtime/src/engine/capacity_flow.rs` — 3 emit sites + shared
  `emit_capacity_event` helper (6.1).
- `crates/agent-runtime/src/engine/mod.rs` (Engine struct definition) —
  `telemetry_sink: Option<TelemetrySink>` field + setter (6.1).
- `crates/agent-runtime/src/telemetry.rs` — `set_enabled` (6.2);
  `RedactedAnalyticsMetadata` + `redact` + `Display` (6.3).
- `crates/agent-runtime/src/events.rs` — `replay_outcome`/`error` →
  `RedactedAnalyticsMetadata` (6.3).
- `crates/tui/src/main.rs` — `run_interactive`: clone sink handle into the
  engine; `set_enabled` post-merge (6.1, 6.2).
- `docs/rfcs/extra-findings-05-telemetry-scaffolding.md` — mark 5.4 emission
  routing deviation closed.
- `docs/rfcs/extra-findings-00-index.md` — add Plan 06 row.

## Implementation notes

Slices 6.1, 6.2, 6.3 are implemented and tested. The following record where
the implementation deviates from or refines the sketch above; they are logged
here so the doc matches the code.

- **6.1 (sink on `EngineConfig`, not an Engine field).** The plan preferred an
  `Option<TelemetrySink>` field on the **Engine** struct to localize the touch.
  Implementation puts it on **`EngineConfig.telemetry_sink`** instead. Rationale:
  `EngineConfig` derives only `Debug, Clone` (no `Serialize` barrier), already
  holds shared runtime state (`SharedTodoList`, etc.), has an `impl Default`
  so the ~28 explicit construction sites that spread `..Default::default()`
  need no change, and `Engine` exposes `pub config: EngineConfig` so the
  engine body reads `self.config.telemetry_sink` exactly as sketched.
  `TelemetrySink` gained a `Debug` derive (it was `Clone` only) so
  `EngineConfig`'s `Debug` derive keeps compiling. The three emit sites share
  one private helper, named `emit_telemetry(&self, event: &Event)` (the plan
  called it `emit_capacity_event`); it matches the three capacity variants,
  builds a `serde_json::json!({...})`, and forwards to `sink.emit(value)`.
  A `capacity_decision_routes_to_telemetry_sink` test (tempdir sink, drives
  `emit_capacity_decision`, asserts the jsonl payload) covers the routing.
- **6.1 (interactive path only; runtime-API path deferred).** The interactive
  TUI engine is spawned via `build_engine_config` (`ui.rs`), which now sets
  `telemetry_sink: app.telemetry_sink.clone()`. The **runtime-API /
  background-thread** engine is spawned by
  `RuntimeThreadManager::ensure_engine_loaded` (`runtime_threads.rs`), a
  separate subsystem the `App` does not hold; its inline `EngineConfig`
  literal sets `telemetry_sink: None` with a documenting comment. Wiring that
  path is a deferred gap (it would require threading a sink handle into
  `RuntimeThreadManager`); the interactive path — the only path that runs
  `attach()` — is fully wired.
- **6.2 (`set_enabled` takes `bool`).** Signature is
  `pub fn set_enabled(&self, enabled: bool)` (stores into the `Arc<AtomicBool>`
  with `Ordering::Relaxed`). It is called in `run_interactive` after the
  project-config merge: `telemetry_sink.set_enabled(config.telemetry_enabled())`.
  Because the engine's clone shares the same `Arc<AtomicBool>`, flipping the
  host handle's flag is visible to the engine with no engine-side setter —
  this is why 6.2 needs no engine change. A
  `set_enabled_toggles_emission_post_attach` test covers the flip.
- **6.3 (final `redact` rules).** `RedactedAnalyticsMetadata` is a newtype over
  `String` (`Debug, Clone, PartialEq, Eq`, no `From<String>`), constructed only
  via `redact(s: &str) -> Self`. `redact` does **not** call
  `partially_sanitize_unicode` (the plan sketched that); instead it applies two
  regex passes (the `regex` crate is already an `agent-runtime` dependency):
  (1) replace unix-absolute (`/x`), home-relative (`~x`), and windows-drive
  (`C:\x`) path runs with `<path>`; (2) replace any `"…"`, `'…'`, or `` `…` ``
  quoted span (which may carry code/PII) with `<redacted>`. The result is
  truncated to `MAX_LEN = 280` chars (plus a trailing `…`). Regexes are cached
  in `std::sync::OnceLock`. `replay_outcome`/`error` are now
  `Option<RedactedAnalyticsMetadata>` / `RedactedAnalyticsMetadata` in
  `events.rs`, constructed via `redact(...)` at the two `capacity_flow.rs`
  build sites, and emitted via `.as_str()`. The `events.rs` test was renamed
  `capacity_intervention_replay_outcome_is_redacted` and now asserts the
  quoted code span is scrubbed; a `redact_strips_paths_quotes_and_truncates`
  unit test covers paths, quotes, and truncation.
