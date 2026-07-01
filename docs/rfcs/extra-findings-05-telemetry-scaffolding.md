# Plan 05: Telemetry Scaffolding + Local jsonl Sink

**Findings:** 2 (trust-timed telemetry init, remainder) + 5a (type barrier) + 5c (ephemeral session id)
**Status:** Implemented — slices 5.1, 5.2 (newtype + apply), 5.3, 5.4 complete and tested. The 5.4 emission-routing, post-merge flag, and 5.2-apply diagnostic-field deferrals recorded below are closed by Plan 06.
**Depends on:** none
**Blocks:** none

## Context

CodeSmith ships **no external telemetry sink** by design — every `reqwest.post`
targets an LLM provider, MCP server, web-search provider, localhost runtime,
sandbox, hook webhook, or OAuth endpoint; there is no Statsig/Sentry/Amplitude
analytics endpoint. The `telemetry` config flag is a boolean recorded into the
prompt payload and passed to child processes; it gates nothing. Per the
approved scope, we build the defensive scaffolding **and** a minimal
**local-only** jsonl telemetry sink so the gating, type barrier, and ephemeral
id have something concrete to protect.

Finding 2's trust-timed init is already largely implemented per
`docs/STARTUP_TRUST_BOUNDARY_AUDIT.md` (early `.env` removal, project-config
overlay gating, `SessionStart` hook deferral). The remainder here is the
audit's candidate #4 — extract named startup phase helpers — plus the
telemetry-sink attach that must happen post-trust.

## Deliverables

### 5.1 Local jsonl telemetry sink — `crates/agent-runtime/src/telemetry.rs`

```rust
pub struct TelemetrySink {
    enabled: Arc<AtomicBool>,
    sink_path: Option<PathBuf>,
    queue: Arc<Mutex<VecDeque<serde_json::Value>>>,
    attached: Arc<AtomicBool>,
}

/// Pre-trust: register the queue, do NOT emit.
pub fn new_skeleton(enabled: bool, sink_path: Option<PathBuf>) -> Self

/// Post-trust: set attached = true, drain the queue to the jsonl file.
pub fn attach(&self)

/// Emit one event. Pre-attach → enqueue; post-attach → drain queue then write.
pub fn emit(&self, event: serde_json::Value)
```

- Sink path: `~/.codesmith/telemetry/events.jsonl` (local only, never network).
- Gated by the existing `telemetry: bool` config (`enabled`).
- `attach()` is called only after the workspace trust check passes
  (the trust gate from `tui/onboarding/mod.rs:155` `needs_trust` /
  `workspace_trust::is_workspace_trusted`).

### 5.2 Type barrier (5a) — `VerifiedAnalyticsMetadata`

In `telemetry.rs`:

```rust
/// Privacy type barrier mirroring Claude Code's
/// `AnalyticsMetadata_I_VERIFIED_THIS_IS_NOT_CODE_OR_FILEPATHS`.
/// The absence of `From<String>` forces conscious construction.
pub struct VerifiedAnalyticsMetadata(pub String);

impl VerifiedAnalyticsMetadata {
    /// Caller asserts this value is NOT code, a file path, or PII.
    pub fn verified(s: &str) -> Self { Self(s.to_string()) }
    pub fn as_str(&self) -> &str { &self.0 }
}
// NOTE: deliberately NO `impl From<String> for VerifiedAnalyticsMetadata`.
```

Every string field assembled into a `TelemetrySink` event must be a
`VerifiedAnalyticsMetadata`. Apply the same barrier to the string fields of the
in-process capacity events (`crates/agent-runtime/src/events.rs:165-202`
`CapacityDecision` / `CapacityIntervention` / `CapacityMemoryPersistFailed`).

### 5.3 Ephemeral telemetry session id (5c)

- Add `telemetry_session_id: String` to `Engine` (or `Session`), minted once
  at construction via `Uuid::new_v4()`. **Not persisted.**
- Sink events' `session_id` field = `telemetry_session_id` (not the resume
  thread id).
- `crates/agent-runtime/src/hooks.rs:237`: `DEEPSEEK_SESSION_ID` now carries
  the **ephemeral** id (behavior change — hooks can no longer correlate across
  restarts via this var). **Also expose `DEEPSEEK_THREAD_ID`** carrying the
  persistent thread id so hook authors can choose.
- `crates/agent-runtime/src/engine/capacity_flow.rs:323/365/953`: the `Event`
  fields use `telemetry_session_id`.
- **Keep the persistent `Session.id` for**: `capacity_flow.rs:948-949` (the
  on-disk capacity-memory filename — `capacity_memory.rs:148`), and
  `engine/mod.rs:619` (resume). This preserves cross-session capacity-memory
  continuity and resume.

### 5.4 Startup phase helpers (finding 2 remainder)

Per `STARTUP_TRUST_BOUNDARY_AUDIT.md` candidate #4, extract named helpers in
`crates/tui/src/main.rs` and have `run_interactive()` call them in order:

1. `init_process_pre_trust()` — process hardening, panic hook, signals.
2. `parse_cli_and_load_user_config()` — CLI args, global config, logging.
3. `resolve_workspace_trust()` — workspace resolution + trust check.
4. `init_project_post_trust()` — workspace `.env`, project config overlay,
   MCP/skills/tools setup, **`TelemetrySink::attach()`**.
5. `dispatch_runtime()` — runtime dispatch.

The sink `attach()` happens inside `init_project_post_trust()` (post-trust).

## Tests

- `TelemetrySink`: pre-`attach` events are queued; post-`attach` drains the
  queue to the jsonl file; a new event after attach is written directly.
- Trust gate: when the workspace is untrusted, `attach()` is not called and no
  jsonl is written.
- `VerifiedAnalyticsMetadata` has no `From<String>` (compile-fail test or
  doc-test) and must be constructed via `verified()`.
- Ephemeral id: regenerated on each `Engine` construction; differs from the
  resume thread id; not persisted across resume.
- Startup helpers: untrusted workspace → sink not attached; trusted → attached.

## Risk

`DEEPSEEK_SESSION_ID` becoming ephemeral is a hook-author behavior change.
Mitigations:

- Also expose `DEEPSEEK_THREAD_ID` (persistent) alongside.
- Document the split in `docs/MEMORY.md`, `docs/OPERATIONS_RUNBOOK.md`, and
  the hooks reference.

## Stop rules

- Do not add any networked telemetry sink — local jsonl only.
- Do not persist the `telemetry_session_id`.
- Do not change resume semantics: `engine/mod.rs:619` and the capacity-memory
  filename (`capacity_memory.rs:148`) keep the persistent thread id.
- Do not move startup actions that the audit classified pre-trust-safe.

## Implementation notes (post-implementation deviations)

Slices 5.1–5.4 are implemented and tested. The following refine the original
sketch above; they are recorded here so the doc matches the code.

- **5.2-apply (selective wrapping).** The plan said *every* string field
  assembled into a sink/capacity event must be `VerifiedAnalyticsMetadata`. In
  practice `CapacityIntervention.replay_outcome` and
  `CapacityMemoryPersistFailed.error` were deliberately left as `String`:
  `replay_outcome` embeds summarized tool outputs
  (`"output_mismatch: original='…' replay='…'"`) and `error` is an IO-error
  summary, so both can carry code or file paths. Wrapping them would assert
  `verified()` over untrusted content, defeating the barrier. Only fields
  that are genuinely safe (ids, enum-derived `risk_band`/`action`,
  controlled-vocabulary `reason`) were wrapped. A `Display` impl was added
  (delegates to the inner string) so TUI `format!("{action}")` keeps working;
  `Display` does not add `From<String>`, so the construction barrier is
  preserved. **Closed by Plan 06/6.3:** these two fields are now
  `RedactedAnalyticsMetadata`, constructed via `redact(...)` at the build site
  (`capacity_flow.rs`), so untrusted diagnostics are sanitized before reaching
  the sink.
- **5.4 (sink kept in `run_interactive` scope).** The plan sketched five
  extracted startup helpers (`init_process_pre_trust`, …, `dispatch_runtime`).
  The locked decision was to keep the sink in `run_interactive` scope and not
  thread it into the engine/App, which removes the justification for the full
  extraction (the post-trust block is also not self-contained — its locals
  flow into `run_tui`). 5.4 was therefore implemented as the two genuinely
  useful, testable helpers — `telemetry_sink_path()` and
  `attach_telemetry_if_trusted(&sink, &boundary)` — plus inline construct
  (pre-trust) and attach (post-trust) in `run_interactive`. The larger
  extraction is deferred. `attach()` on an empty queue writes no file, so the
  wiring has no disk side effect until emission is routed.
- **5.4 (emission routing deferred).** The sink is constructed and attached
  but is not yet handed to the engine, so no `emit()` calls occur this plan.
  This was intentional scaffolding: the trust-timed attach, the config flag,
  and the type barrier were in place; routing capacity events to `sink.emit()`
  was a follow-up. **Closed by Plan 06/6.1:** the sink is now threaded into
  the engine via `EngineConfig.telemetry_sink`, and the three capacity-event
  construction sites emit JSON via a shared `emit_telemetry` helper.
- **5.3 (non-breaking `HookHost::session_id`).** The `HookHost::session_id()`
  trait method and its `HookExecutor` impl were left intact (still exercised
  by a TUI test). The engine's two call sites were rewired to read
  `session.telemetry_session_id` / `session.id` directly, so
  `DEEPSEEK_SESSION_ID` carries the ephemeral id without changing the trait
  surface.
- **5.4 (TUI `telemetry` config flag).** The `telemetry: Option<bool>` flag
  already existed in `codesmith_config` per-provider options but was not
  surfaced by the TUI `Config`. A top-level `#[serde(default)]
  telemetry: Option<bool>` + `telemetry_enabled()` was added to
  `crates/tui/src/config.rs` (mirroring `allow_shell`), and `merge_config`
  was updated to carry it through the project-config overlay. **Closed by
  Plan 06/6.2:** because the sink is constructed pre-merge, its `enabled`
  flag is re-applied post-merge via
  `TelemetrySink::set_enabled(config.telemetry_enabled())` in
  `run_interactive`; the `Arc<AtomicBool>` is shared with the engine's clone,
  so no engine-side setter is needed.

## Files

- `crates/agent-runtime/src/telemetry.rs` (new) — `TelemetrySink` (5.1),
  `VerifiedAnalyticsMetadata` newtype + `Display` (5.2-newtype/apply).
- `crates/agent-runtime/src/lib.rs` — module registration (5.1).
- `crates/agent-runtime/src/session.rs` — ephemeral
  `telemetry_session_id` field, minted in `Session::new()` (5.3).
- `crates/agent-runtime/src/engine/mod.rs` — `build_compaction_enhancements`
  rewired to `telemetry_session_id`/`thread_id`; resume + `emit_session_updated`
  keep the persistent id (5.3).
- `crates/agent-runtime/src/engine/capacity_flow.rs` — capacity `Event`
  fields use `telemetry_session_id` + `VerifiedAnalyticsMetadata` (5.3,
  5.2-apply); on-disk capacity-memory filename keeps `session.id` (5.3).
- `crates/agent-runtime/src/engine/turn_loop.rs` — `ToolHookEnv` gained
  `telemetry_session_id`/`thread_id`; `build_tool_hook_context` no longer
  takes the hook executor (5.3).
- `crates/agent-runtime/src/events.rs` — `CapacityDecision` /
  `CapacityIntervention` / `CapacityMemoryPersistFailed` safe string fields
  → `VerifiedAnalyticsMetadata`; `replay_outcome`/`error` left `String`
  (5.2-apply).
- `crates/agent-runtime/src/hooks.rs` — `HookContext.thread_id` +
  `DEEPSEEK_THREAD_ID`; `session_id` doc updated to ephemeral (5.3).
- `crates/tui/src/hooks.rs` — `pre_compact` + `message_submit` payloads
  carry `thread_id` (5.3).
- `crates/tui/src/config.rs` — top-level `telemetry: Option<bool>` +
  `telemetry_enabled()`; `merge_config` carries it through (5.4).
- `crates/tui/src/main.rs` — `TelemetrySink` import,
  `telemetry_sink_path()`, `attach_telemetry_if_trusted()`, inline
  construct/attach in `run_interactive`, `telemetry_startup_tests` (5.4).
- `crates/tui/src/main.rs` (startup helpers)
- `docs/MEMORY.md`, `docs/OPERATIONS_RUNBOOK.md`
